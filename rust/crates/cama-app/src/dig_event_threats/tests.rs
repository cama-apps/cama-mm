use std::sync::Mutex;

use super::*;
use cama_db::dig_event_threats::PersistedCurse;

const ACTOR: i64 = 10_001;
const GUILD: i64 = 12_345;

#[derive(Clone, Debug)]
struct FakeState {
    snapshot: Option<ThreatSnapshot>,
    actions: usize,
}

#[derive(Debug)]
struct FakePort {
    state: Mutex<FakeState>,
}

impl FakePort {
    fn new(streak_days: i64, balance: i64) -> Self {
        Self {
            state: Mutex::new(FakeState {
                snapshot: Some(ThreatSnapshot {
                    key: key(),
                    depth: 50,
                    streak_days,
                    luminosity: 100,
                    temp_curse_json: None,
                    temp_curse: PersistedCurse::Absent,
                    temp_buff_json: None,
                    balance,
                }),
                actions: 0,
            }),
        }
    }

    fn current(&self) -> ThreatSnapshot {
        self.state
            .lock()
            .expect("fake state")
            .snapshot
            .clone()
            .expect("seeded tunnel")
    }

    fn set_buff(&self, raw: &str) {
        self.state
            .lock()
            .expect("fake state")
            .snapshot
            .as_mut()
            .expect("seeded tunnel")
            .temp_buff_json = Some(raw.to_owned());
    }
}

impl DigEventThreatPort for FakePort {
    type Error = &'static str;

    fn snapshot(&self, requested: ThreatTunnelKey) -> Result<Option<ThreatSnapshot>, Self::Error> {
        let state = self.state.lock().map_err(|_| "poisoned")?;
        Ok(state
            .snapshot
            .as_ref()
            .filter(|snapshot| snapshot.key == requested)
            .cloned())
    }

    fn replace_temp_curse(
        &self,
        requested: ThreatTunnelKey,
        curse: Option<&ThreatCurse>,
    ) -> Result<ReplaceCurseOutcome, Self::Error> {
        let mut state = self.state.lock().map_err(|_| "poisoned")?;
        let Some(snapshot) = state
            .snapshot
            .as_mut()
            .filter(|snapshot| snapshot.key == requested)
        else {
            return Ok(ReplaceCurseOutcome::MissingTunnel);
        };
        snapshot.temp_curse = curse
            .cloned()
            .map_or(PersistedCurse::Absent, PersistedCurse::Stored);
        snapshot.temp_curse_json = curse.map(|curse| format!("curse:{}", curse.id));
        Ok(ReplaceCurseOutcome::Applied)
    }

    fn settle_atomic(
        &self,
        request: AtomicThreatSettlement<'_>,
    ) -> Result<ThreatSettlementOutcome, Self::Error> {
        let mut state = self.state.lock().map_err(|_| "poisoned")?;
        let Some(current) = state.snapshot.as_mut() else {
            return Ok(ThreatSettlementOutcome::MissingTunnel);
        };
        if current != request.expected {
            return Ok(ThreatSettlementOutcome::Conflict);
        }
        let before = current.balance;
        current.depth = request.depth_after;
        current.streak_days = request.streak_days_after;
        current.luminosity = request.luminosity_after;
        match request.curse_mutation {
            ThreatCurseMutation::Preserve => {}
            ThreatCurseMutation::Clear => {
                current.temp_curse = PersistedCurse::Absent;
                current.temp_curse_json = None;
            }
            ThreatCurseMutation::Replace(curse) => {
                current.temp_curse = PersistedCurse::Stored(curse.clone());
                current.temp_curse_json = Some(format!("curse:{}", curse.id));
            }
        }
        current.balance = current.balance.saturating_add(request.balance_delta);
        let after = current.balance;
        state.actions += 1;
        Ok(ThreatSettlementOutcome::Applied {
            balance_before: before,
            balance_after: after,
        })
    }
}

fn key() -> ThreatTunnelKey {
    ThreatTunnelKey {
        discord_id: ACTOR,
        guild_id: Some(GUILD),
    }
}

fn outcome(jc: i64, streak_loss: i64, curse: Option<TempCurseDefinition>) -> ThreatEventOutcome {
    ThreatEventOutcome {
        description: "Synthetic outcome".to_owned(),
        jc,
        streak_loss,
        curse,
        ..ThreatEventOutcome::default()
    }
}

fn option(failure: ThreatEventOutcome) -> ThreatEventOption {
    ThreatEventOption {
        success_chance: 0.5,
        success: outcome(20, 0, None),
        failure: Some(failure),
    }
}

fn slow_curse() -> TempCurseDefinition {
    TempCurseDefinition {
        id: "test_slow_hex".to_owned(),
        name: "Slowing Hex".to_owned(),
        duration_digs: 2,
        effects: ThreatCurseEffects {
            advance_bonus: Some(-3),
            ..ThreatCurseEffects::default()
        },
    }
}

fn resolve(
    service: &DigEventThreatService<FakePort>,
    event_option: &ThreatEventOption,
    choice: EventThreatChoice,
    success_roll: f64,
) -> ThreatResolution {
    let result = service
        .resolve_event(ResolveThreatRequest {
            key: key(),
            event_id: "synthetic",
            choice,
            option: event_option,
            rolls: ThreatRolls {
                success_roll,
                ..ThreatRolls::default()
            },
            minigame_jc_delta_scale: default_economy_scale(),
            now: 1_000_000,
        })
        .expect("resolve event");
    match result {
        ResolveThreatOutcome::Applied(resolution) => *resolution,
        other => panic!("unexpected resolution: {other:?}"),
    }
}

mod test_streak_threat {
    use super::*;

    fn streak_failure(streak: i64, loss: i64) -> (ThreatResolution, ThreatSnapshot) {
        let service = DigEventThreatService::new(FakePort::new(streak, 500));
        let resolution = resolve(
            &service,
            &option(outcome(0, loss, None)),
            EventThreatChoice::Risky,
            0.99,
        );
        (resolution, service.port().current())
    }

    #[test]
    fn test_streak_setback_applies_and_is_partial() {
        let (result, tunnel) = streak_failure(10, 3);
        assert_eq!(tunnel.streak_days, 7);
        assert_eq!(result.streak_loss, 3);
    }

    #[test]
    fn test_streak_setback_scales_with_streak_length() {
        let (result, tunnel) = streak_failure(45, 3);
        assert_eq!(tunnel.streak_days, 40);
        assert_eq!(result.streak_loss, 5);
    }

    #[test]
    fn test_streak_setback_never_resets_to_zero() {
        let (result, tunnel) = streak_failure(2, 3);
        assert_eq!(tunnel.streak_days, 0);
        assert_eq!(result.streak_loss, 2);
    }

    #[test]
    fn test_streak_setback_leaves_long_streak_buffer() {
        let (result, tunnel) = streak_failure(60, 2);
        assert_eq!(tunnel.streak_days, 55);
        assert_eq!(result.streak_loss, 5);
    }

    #[test]
    fn test_risky_success_leaves_streak_untouched() {
        let service = DigEventThreatService::new(FakePort::new(10, 500));
        let result = resolve(
            &service,
            &option(outcome(0, 5, None)),
            EventThreatChoice::Risky,
            0.01,
        );
        assert!(result.succeeded);
        assert_eq!(result.streak_loss, 0);
        assert_eq!(service.port().current().streak_days, 10);
    }
}

mod test_curse_threat {
    use super::*;

    #[test]
    fn test_new_fractional_effect_keys_are_valid() {
        let effects = ThreatCurseEffects {
            cave_in_bonus: Some(0.08),
            cooldown_penalty: Some(0.20),
            ..ThreatCurseEffects::default()
        };
        assert_eq!(effects.cave_in_bonus, Some(0.08));
        assert_eq!(effects.cooldown_penalty, Some(0.20));
    }

    #[test]
    fn test_fractional_curse_scaling_stays_fractional_and_integers_round() {
        let scaled = scale_curse_effects(
            ThreatCurseEffects {
                cave_in_bonus: Some(0.08),
                cooldown_penalty: Some(0.20),
                advance_bonus: Some(-3),
                ..ThreatCurseEffects::default()
            },
            1.3,
        );
        assert!((scaled.cave_in_bonus.expect("cave-in") - 0.104).abs() < 1e-12);
        assert!((scaled.cooldown_penalty.expect("cooldown") - 0.26).abs() < 1e-12);
        assert_eq!(scaled.advance_bonus, Some(-4));
    }

    #[test]
    fn test_capped_fractional_effects_do_not_exceed_active_limits() {
        let effects = ThreatCurseEffects {
            cave_in_bonus: Some(0.104),
            cooldown_penalty: Some(0.26),
            ..ThreatCurseEffects::default()
        };
        assert_eq!(
            capped_curse_effect(effects, FractionalCurseEffect::CaveIn),
            0.10
        );
        assert_eq!(
            capped_curse_effect(effects, FractionalCurseEffect::Cooldown),
            0.25
        );
    }

    #[test]
    fn test_cooldown_curse_applies_after_stamina_and_caps_at_twenty_five_percent() {
        let effects = ThreatCurseEffects {
            cooldown_penalty: Some(0.30),
            ..ThreatCurseEffects::default()
        };
        assert_eq!(cursed_cooldown(4_000, effects, 0), 5_000);
    }

    #[test]
    fn test_cave_in_curse_matches_live_and_precondition_paths() {
        let effects = ThreatCurseEffects {
            cave_in_bonus: Some(0.50),
            ..ThreatCurseEffects::default()
        };
        let preview = cursed_cave_in_chance(0.05, effects);
        let live = cursed_cave_in_chance(0.05, effects);
        assert_eq!(preview, live);
        assert!(preview > cursed_cave_in_chance(0.05, ThreatCurseEffects::default()));
    }

    #[test]
    fn test_curse_applied_and_persisted() {
        let service = DigEventThreatService::new(FakePort::new(0, 500));
        let result = resolve(
            &service,
            &option(outcome(0, 0, Some(slow_curse()))),
            EventThreatChoice::Risky,
            0.99,
        );
        assert_eq!(
            result.curse_applied.as_ref().map(|curse| curse.id.as_str()),
            Some("test_slow_hex")
        );
        let active = service.port().current().temp_curse;
        let curse = active.active().expect("persisted active curse");
        assert_eq!(curse.digs_remaining, 4);
        assert_eq!(curse.effects.advance_bonus, Some(-4));
    }

    #[test]
    fn test_curse_does_not_clobber_active_buff() {
        let service = DigEventThreatService::new(FakePort::new(0, 500));
        service
            .port()
            .set_buff(r#"{"id":"power","digs_remaining":3}"#);
        resolve(
            &service,
            &option(outcome(0, 0, Some(slow_curse()))),
            EventThreatChoice::Risky,
            0.99,
        );
        let current = service.port().current();
        assert!(current.temp_curse.active().is_some());
        assert_eq!(
            current.temp_buff_json.as_deref(),
            Some(r#"{"id":"power","digs_remaining":3}"#)
        );
    }

    #[test]
    fn test_curse_advance_drain_reduces_a_dig() {
        let cursed = apply_curse_to_dig(
            3,
            2,
            100,
            ThreatCurseEffects {
                advance_bonus: Some(-1),
                ..ThreatCurseEffects::default()
            },
        );
        let clean = apply_curse_to_dig(3, 2, 100, ThreatCurseEffects::default());
        assert_eq!(cursed.advance, clean.advance - 1);
    }

    #[test]
    fn test_curse_jc_drain_reduces_earnings_floored_at_zero() {
        let result = apply_curse_to_dig(
            3,
            5,
            100,
            ThreatCurseEffects {
                jc_bonus: Some(-9_999),
                ..ThreatCurseEffects::default()
            },
        );
        assert_eq!(result.jc_earned, 0);
    }

    #[test]
    fn test_curse_luminosity_drain_burns_extra_light() {
        let cursed = apply_curse_to_dig(
            3,
            2,
            100,
            ThreatCurseEffects {
                luminosity_drain: Some(15),
                ..ThreatCurseEffects::default()
            },
        );
        let clean = apply_curse_to_dig(3, 2, 100, ThreatCurseEffects::default());
        assert_eq!(cursed.luminosity_after, clean.luminosity_after - 15);
    }

    #[test]
    fn test_curse_decrements_each_dig_and_expires() {
        let service = DigEventThreatService::new(FakePort::new(0, 500));
        let mut short = slow_curse();
        short.duration_digs = 2;
        service.set_temp_curse(key(), &short).expect("set curse");
        let first = service
            .apply_cursed_dig(
                key(),
                CursedDigInput {
                    base_advance: 3,
                    base_jc: 0,
                    clean_luminosity_after: 100,
                    now: 1,
                },
            )
            .expect("first dig");
        assert!(matches!(
            first,
            CursedDigOutcome::Applied(CursedDigApplied {
                curse_remaining: Some(1),
                ..
            })
        ));
        service
            .apply_cursed_dig(
                key(),
                CursedDigInput {
                    base_advance: 3,
                    base_jc: 0,
                    clean_luminosity_after: 100,
                    now: 2,
                },
            )
            .expect("second dig");
        assert!(service.port().current().temp_curse.active().is_none());
    }

    #[test]
    fn test_expired_curse_no_longer_drains() {
        let expired = ThreatCurse {
            id: "expired".to_owned(),
            name: "Expired".to_owned(),
            digs_remaining: 0,
            effects: ThreatCurseEffects {
                advance_bonus: Some(-2),
                ..ThreatCurseEffects::default()
            },
        };
        let persisted = PersistedCurse::Stored(expired);
        let effects = persisted
            .active()
            .map_or_else(ThreatCurseEffects::default, |curse| curse.effects);
        assert_eq!(apply_curse_to_dig(3, 0, 100, effects).advance, 3);
    }

    #[test]
    fn test_desperate_failure_applies_curse() {
        let service = DigEventThreatService::new(FakePort::new(0, 500));
        let result = resolve(
            &service,
            &option(outcome(0, 0, Some(slow_curse()))),
            EventThreatChoice::Desperate,
            0.99,
        );
        assert_eq!(result.choice, EventThreatChoice::Desperate);
        assert_eq!(
            service
                .port()
                .current()
                .temp_curse
                .active()
                .map(|curse| curse.digs_remaining),
            Some(4)
        );
    }

    #[test]
    fn test_desperate_failure_applies_streak_loss() {
        let service = DigEventThreatService::new(FakePort::new(10, 500));
        let result = resolve(
            &service,
            &option(outcome(0, 3, None)),
            EventThreatChoice::Desperate,
            0.99,
        );
        assert_eq!(result.streak_loss, 3);
        assert_eq!(service.port().current().streak_days, 7);
    }

    #[test]
    fn test_second_curse_replaces_first_cleanly() {
        let service = DigEventThreatService::new(FakePort::new(0, 500));
        let mut first = slow_curse();
        first.id = "first_hex".to_owned();
        first.duration_digs = 5;
        let second = TempCurseDefinition {
            id: "second_hex".to_owned(),
            name: "Second Hex".to_owned(),
            duration_digs: 2,
            effects: ThreatCurseEffects {
                jc_bonus: Some(-10),
                ..ThreatCurseEffects::default()
            },
        };
        service.set_temp_curse(key(), &first).expect("first curse");
        service
            .set_temp_curse(key(), &second)
            .expect("second curse");
        let current = service.port().current();
        let active = current.temp_curse.active().expect("second active curse");
        assert_eq!(active.id, "second_hex");
        assert_eq!(active.digs_remaining, 2);
        assert_eq!(active.effects, second.effects);
    }

    #[test]
    fn test_second_curse_via_event_replaces_first() {
        let service = DigEventThreatService::new(FakePort::new(0, 500));
        let mut long = slow_curse();
        long.id = "long_hex".to_owned();
        long.duration_digs = 6;
        let short = TempCurseDefinition {
            id: "short_hex".to_owned(),
            name: "Short Hex".to_owned(),
            duration_digs: 1,
            effects: ThreatCurseEffects {
                jc_bonus: Some(-5),
                ..ThreatCurseEffects::default()
            },
        };
        resolve(
            &service,
            &option(outcome(0, 0, Some(long))),
            EventThreatChoice::Risky,
            0.99,
        );
        resolve(
            &service,
            &option(outcome(0, 0, Some(short))),
            EventThreatChoice::Risky,
            0.99,
        );
        let current = service.port().current();
        let active = current.temp_curse.active().expect("short curse");
        assert_eq!(active.id, "short_hex");
        assert_eq!(active.digs_remaining, 3);
        assert_eq!(active.effects.jc_bonus, Some(-8));
        assert_eq!(active.effects.advance_bonus, None);
    }
}

mod test_jc_threat {
    use super::*;

    #[test]
    fn test_jc_threat_failure_can_take_balance_negative() {
        let service = DigEventThreatService::new(FakePort::new(0, 30));
        let result = resolve(
            &service,
            &option(outcome(-200, 0, None)),
            EventThreatChoice::Risky,
            0.99,
        );
        let expected = scale_deflationary_minigame_jc_delta(
            strengthen_dig_event_penalty(-260) as f64,
            default_economy_scale(),
        );
        assert_eq!(result.jc_delta, expected);
        assert_eq!(service.port().current().balance, 30 + expected);
        assert!(service.port().current().balance < 0);
        assert_eq!(result.balance_after, Some(30 + expected));
    }

    #[test]
    fn test_jc_threat_success_pays_out_normally() {
        let service = DigEventThreatService::new(FakePort::new(0, 30));
        let mut event_option = option(outcome(-200, 0, None));
        event_option.success.jc = 40;
        let result = resolve(&service, &event_option, EventThreatChoice::Risky, 0.01);
        let expected =
            scale_positive_dig_jc(scale_minigame_jc_delta(40.0, default_economy_scale()));
        assert_eq!(result.jc_delta, expected);
        assert_eq!(service.port().current().balance, 30 + expected);
        assert_eq!(result.balance_after, None);
    }
}

mod test_event_result_embed {
    use super::*;

    fn field_map(embed: &EmbedModel) -> Vec<(&str, &str)> {
        embed
            .fields
            .iter()
            .map(|field| (field.name.as_str(), field.value.as_str()))
            .collect()
    }

    #[test]
    fn test_embed_surfaces_streak_loss() {
        let embed = build_threat_result_embed(&ThreatResolution {
            choice: EventThreatChoice::Risky,
            succeeded: false,
            message: "Momentum lost".to_owned(),
            depth_delta: 0,
            jc_delta: 0,
            streak_loss: 3,
            curse_applied: None,
            balance_after: None,
        });
        let fields = field_map(&embed);
        assert!(
            fields
                .iter()
                .any(|(name, value)| { *name == "Outcome" && value.contains("3 streak days") })
        );
    }

    #[test]
    fn test_embed_surfaces_curse() {
        let embed = build_threat_result_embed(&ThreatResolution {
            choice: EventThreatChoice::Risky,
            succeeded: false,
            message: "Hexed".to_owned(),
            depth_delta: 0,
            jc_delta: 0,
            streak_loss: 0,
            curse_applied: Some(slow_curse()),
            balance_after: None,
        });
        let fields = field_map(&embed);
        assert!(fields.iter().any(|(name, value)| {
            name.starts_with("Curse:") && name.contains("Slowing Hex") && value.contains("2 digs")
        }));
    }

    #[test]
    fn test_embed_surfaces_debt() {
        let embed = build_threat_result_embed(&ThreatResolution {
            choice: EventThreatChoice::Risky,
            succeeded: false,
            message: "Pockets emptied".to_owned(),
            depth_delta: 0,
            jc_delta: -200,
            streak_loss: 0,
            curse_applied: None,
            balance_after: Some(-170),
        });
        assert!(
            field_map(&embed)
                .iter()
                .any(|(name, value)| { *name == "In Debt" && value.contains("red") })
        );
    }

    #[test]
    fn test_embed_omits_threat_fields_on_clean_success() {
        let embed = build_threat_result_embed(&ThreatResolution {
            choice: EventThreatChoice::Risky,
            succeeded: true,
            message: "Clean win".to_owned(),
            depth_delta: 0,
            jc_delta: 13,
            streak_loss: 0,
            curse_applied: None,
            balance_after: None,
        });
        let fields = field_map(&embed);
        assert!(!fields.iter().any(|(name, _)| name.starts_with("Curse:")));
        assert!(!fields.iter().any(|(name, _)| *name == "In Debt"));
        assert!(!fields.iter().any(|(_, value)| value.contains("streak")));
    }
}
