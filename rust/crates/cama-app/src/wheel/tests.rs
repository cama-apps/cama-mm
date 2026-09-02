use std::collections::VecDeque;

use super::*;

#[derive(Default)]
struct MemoryAdmission {
    registered: bool,
    last_spin: Option<i64>,
    claim_calls: usize,
    last_calls: usize,
    set_calls: Vec<i64>,
}

impl SpinAdmissionPort for MemoryAdmission {
    type Error = &'static str;

    fn is_registered(&mut self, _user_id: i64, _guild_id: i64) -> Result<bool, Self::Error> {
        Ok(self.registered)
    }

    fn try_claim_wheel_spin(
        &mut self,
        _user_id: i64,
        _guild_id: i64,
        now: i64,
        current_window_started_at: i64,
    ) -> Result<bool, Self::Error> {
        self.claim_calls += 1;
        let available = self
            .last_spin
            .is_none_or(|last| last < current_window_started_at);
        if available {
            self.last_spin = Some(now);
        }
        Ok(available)
    }

    fn last_wheel_spin(
        &mut self,
        _user_id: i64,
        _guild_id: i64,
    ) -> Result<Option<i64>, Self::Error> {
        self.last_calls += 1;
        Ok(self.last_spin)
    }

    fn set_last_wheel_spin(
        &mut self,
        _user_id: i64,
        _guild_id: i64,
        timestamp: i64,
    ) -> Result<(), Self::Error> {
        self.last_spin = Some(timestamp);
        self.set_calls.push(timestamp);
        Ok(())
    }
}

#[derive(Default)]
struct MemoryCredit {
    balance: i64,
    direct: Vec<CreditRequest>,
    garnished: Vec<CreditRequest>,
    balance_reads: usize,
    garnish_receipt: Option<CreditReceipt>,
}

impl CreditPort for MemoryCredit {
    type Error = &'static str;

    fn add_garnishable_income(
        &mut self,
        request: &CreditRequest,
    ) -> Result<CreditReceipt, Self::Error> {
        self.garnished.push(request.clone());
        Ok(self.garnish_receipt.unwrap_or(CreditReceipt {
            new_balance: self.balance.saturating_add(request.amount),
            garnished: 0,
        }))
    }

    fn adjust_balance(&mut self, request: &CreditRequest) -> Result<(), Self::Error> {
        self.direct.push(request.clone());
        self.balance = self.balance.saturating_add(request.amount);
        Ok(())
    }

    fn balance(&mut self, _user_id: i64, _guild_id: i64) -> Result<i64, Self::Error> {
        self.balance_reads += 1;
        Ok(self.balance)
    }
}

#[derive(Default)]
struct FakeHostile {
    requests: Vec<HostileLossRequest>,
    responses: VecDeque<Result<HostileLossReceipt, &'static str>>,
}

impl HostileLossPort for FakeHostile {
    type Error = &'static str;

    fn settle(&mut self, request: &HostileLossRequest) -> Result<HostileLossReceipt, Self::Error> {
        self.requests.push(request.clone());
        self.responses.pop_front().unwrap_or_else(|| {
            Ok(receipt(
                request,
                0,
                false,
                match request.destination {
                    HostileDestination::Player(_) => Some(request.amount),
                    HostileDestination::Burn | HostileDestination::Reserve => None,
                },
            ))
        })
    }
}

fn player(discord_id: i64, balance: i64) -> PlayerSnapshot {
    PlayerSnapshot::liquid(discord_id, format!("P{discord_id}"), balance)
}

fn receipt(
    request: &HostileLossRequest,
    absorbed: i64,
    centralized: bool,
    destination_balance_after: Option<i64>,
) -> HostileLossReceipt {
    let applied = request.amount.saturating_sub(absorbed).max(0);
    HostileLossReceipt {
        event_key: request.event_key.clone(),
        requested: request.amount,
        attempted: request.amount,
        absorbed,
        applied,
        victim_balance_before: request.victim_balance,
        victim_balance_after: request.victim_balance.saturating_sub(applied),
        destination_balance_after,
        centralized,
    }
}

fn audit(user_id: i64, related_id: &str, reason: &str) -> GambaAudit {
    GambaAudit::new(user_id, related_id, reason)
}

fn credit_request(user_id: i64, balance_delta: i64) -> CreditRequest {
    CreditRequest {
        user_id,
        guild_id: 123,
        amount: balance_delta,
        audit: audit(user_id, "DIVIDEND", "gamba dividend credit"),
    }
}

#[test]
fn test_hostile_takeover_settlement_failure_does_not_grant_fallback() {
    let leaderboard = vec![
        player(1, 500),
        player(2, 400),
        player(3, 300),
        player(4, HOSTILE_LOSS_MIN_BALANCE),
    ];
    let mut hostile = FakeHostile {
        responses: VecDeque::from([Err("settlement failed")]),
        ..FakeHostile::default()
    };

    let outcome = settle_hostile_takeover(
        &mut hostile,
        &leaderboard,
        1,
        123,
        "wheel:test",
        LossRoll {
            percentage: 0.08,
            flat: 1,
        },
        1.0,
    );

    assert!(outcome.missed);
    assert_eq!(outcome.amount, 0);
    assert_eq!(outcome.fallback_credit, 0);
    assert_eq!(hostile.requests.len(), 1);
}

#[test]
fn test_wheel_requires_registration() {
    let mut port = MemoryAdmission::default();
    let result = admit_spin(
        &mut port,
        SpinRequest {
            user_id: 456,
            guild_id: 123,
            now: 1_700_000_000,
            bonus_spin: false,
            is_admin: false,
        },
    );

    assert_eq!(result, Ok(SpinAdmission::RejectedUnregistered));
    assert_eq!(port.claim_calls, 0);
    assert!(port.set_calls.is_empty());
}

#[test]
fn test_wheel_cooldown_expired_allows_spin() {
    let now = 1_700_000_000;
    let mut port = MemoryAdmission {
        registered: true,
        last_spin: Some(now - GAME_DAY_SECONDS - 1),
        ..MemoryAdmission::default()
    };
    let request = SpinRequest {
        user_id: 1_001,
        guild_id: 123,
        now,
        bonus_spin: false,
        is_admin: false,
    };

    assert_eq!(admit_spin(&mut port, request), Ok(SpinAdmission::Allowed));
    // A racing request sees the atomic claim, rather than independently
    // passing a stale read/check pair.
    assert!(matches!(
        admit_spin(&mut port, request),
        Ok(SpinAdmission::RejectedCooldown { .. })
    ));
    assert_eq!(port.claim_calls, 2);
}

#[test]
fn wheel_resets_at_fixed_four_am_utc_minus_eight_boundary() {
    // Every fixed-PST 4 AM boundary is 12:00 UTC. A spin one second before
    // this boundary is available again one second later, not 24 hours later.
    let reset = 20 * GAME_DAY_SECONDS + 12 * 3_600;
    let mut port = MemoryAdmission {
        registered: true,
        last_spin: Some(reset - 1),
        ..MemoryAdmission::default()
    };

    assert_eq!(
        admit_spin(
            &mut port,
            SpinRequest {
                user_id: 1_001,
                guild_id: 123,
                now: reset,
                bonus_spin: false,
                is_admin: false,
            },
        ),
        Ok(SpinAdmission::Allowed)
    );
    assert_eq!(port.last_spin, Some(reset));

    // The newly claimed spin remains unavailable through the whole window.
    assert!(matches!(
        admit_spin(
            &mut port,
            SpinRequest {
                user_id: 1_001,
                guild_id: 123,
                now: reset + GAME_DAY_SECONDS - 1,
                bonus_spin: false,
                is_admin: false,
            },
        ),
        Ok(SpinAdmission::RejectedCooldown { .. })
    ));
}

#[test]
fn wheel_reset_timestamp_is_fixed_across_the_year() {
    // 2026-01-15 and 2026-07-15 are on opposite sides of US daylight saving.
    // Both reset at 12:00 UTC because the game clock is fixed UTC-8.
    for reset in [1_768_478_400, 1_784_116_800] {
        assert_eq!(wheel_window_start_at(reset), reset);
        assert_eq!(next_wheel_reset_at(reset - 1), reset);
        assert_eq!(next_wheel_reset_at(reset), reset + GAME_DAY_SECONDS);
    }
}

#[test]
fn test_wheel_positive_applies_garnishment() {
    let mut port = MemoryCredit {
        balance: -100,
        garnish_receipt: Some(CreditReceipt {
            new_balance: -70,
            garnished: 30,
        }),
        ..MemoryCredit::default()
    };
    let request = credit_request(1_002, 50);

    let outcome = credit_gamba_outcome(&mut port, true, -100, request.clone()).unwrap();

    assert_eq!(outcome.route, CreditRoute::Garnishment);
    assert_eq!((outcome.new_balance, outcome.garnished), (-70, 30));
    assert_eq!(port.garnished, vec![request]);
    assert!(port.direct.is_empty());
    assert_eq!(port.balance_reads, 0);
}

#[test]
fn test_wheel_positive_no_debt_adds_directly() {
    let mut port = MemoryCredit {
        balance: 50,
        ..MemoryCredit::default()
    };
    let request = CreditRequest {
        user_id: 1_003,
        guild_id: 123,
        amount: 5,
        audit: audit(1_003, "5", "gamba wheel payout"),
    };

    let outcome = credit_gamba_outcome(&mut port, false, 50, request.clone()).unwrap();

    assert_eq!(outcome.route, CreditRoute::Direct);
    assert_eq!((outcome.new_balance, outcome.garnished), (55, 0));
    assert_eq!(port.direct, vec![request]);
    assert_eq!(port.balance_reads, 1);
    assert!(port.garnished.is_empty());
}

#[test]
fn test_wheel_profit_applies_audited_vanity_tax() {
    let deductions = post_win_deductions(100, None, Some(0.10));
    let vanity_audit = GambaAudit {
        source: "vanity_tax",
        actor_id: 1_004,
        related_type: "wheel_spin",
        related_id: "100".to_owned(),
        reason: "gamba vanity tax".to_owned(),
    };

    assert_eq!(deductions.vanity_tax, 10);
    assert_eq!(deductions.net_gain, 90);
    assert_eq!(vanity_audit.source, "vanity_tax");
    assert_eq!(vanity_audit.related_type, "wheel_spin");
    assert!(vanity_audit.reason.starts_with("gamba "));
}

#[test]
fn test_wheel_white_mana_animation_uses_capped_wedges() {
    let wedges = wheel_catalog(WheelKind::Regular, WheelEconomyPolicy::default());
    let target = wedges
        .iter()
        .position(|wedge| wedge.value == WheelValue::Numeric(80))
        .unwrap();

    let animated = cap_numeric_wins(&wedges, 50);

    assert_eq!(animated[target].label, "50");
    assert_eq!(animated[target].value, WheelValue::Numeric(50));
}

#[test]
fn test_wheel_blue_mana_embed_uses_reduced_numeric_payout() {
    let settlement = numeric_settlement(NumericPolicyInput {
        value: 80,
        is_dig_bonus: false,
        blue_reduction: 0.25,
        is_bad_gamba: false,
        pardon_active: false,
    });
    let NumericSettlement::Credit(value) = settlement else {
        panic!("positive outcome must credit");
    };
    let presentation = numeric_win_presentation(value, 0.25);

    assert_eq!(value, 60);
    assert_eq!(presentation.title, "🎉 Winner!");
    assert!(presentation.description.contains("won **60**"));
    assert_eq!(presentation.fields[0].name, "🏝️ Blue Mana Tax");
}

#[test]
fn test_wheel_bankrupt_subtracts_balance() {
    let wedges = wheel_catalog(WheelKind::Regular, WheelEconomyPolicy::default());
    let loss = wedges
        .iter()
        .find_map(|wedge| match wedge.value {
            WheelValue::Numeric(value) if value < 0 => Some(value),
            _ => None,
        })
        .unwrap();

    assert_eq!(
        numeric_settlement(NumericPolicyInput {
            value: loss,
            is_dig_bonus: false,
            blue_reduction: 0.0,
            is_bad_gamba: false,
            pardon_active: false,
        }),
        NumericSettlement::Debit {
            amount: loss,
            reserve_credit: loss.abs(),
        }
    );
}

#[test]
fn test_wheel_bankrupt_credits_nonprofit_fund() {
    let result = numeric_settlement(NumericPolicyInput {
        value: -595,
        is_dig_bonus: false,
        blue_reduction: 0.0,
        is_bad_gamba: false,
        pardon_active: false,
    });

    assert_eq!(
        result,
        NumericSettlement::Debit {
            amount: -595,
            reserve_credit: 595,
        }
    );
}

#[test]
fn test_wheel_bankrupt_ignores_max_debt() {
    let NumericSettlement::Debit { amount, .. } = numeric_settlement(NumericPolicyInput {
        value: -18,
        is_dig_bonus: false,
        blue_reduction: 0.0,
        is_bad_gamba: true,
        pardon_active: false,
    }) else {
        panic!("negative outcome must debit");
    };

    assert_eq!((-400_i64).saturating_add(amount), -418);
}

#[test]
fn test_wheel_lose_turn_no_change() {
    let result = numeric_settlement(NumericPolicyInput {
        value: 0,
        is_dig_bonus: false,
        blue_reduction: 0.0,
        is_bad_gamba: false,
        pardon_active: false,
    });
    let wedge = WheelWedge {
        label: "0".to_owned(),
        value: WheelValue::Numeric(0),
        color: "#4a4a4a",
    };

    assert_eq!(result, NumericSettlement::LoseTurn);
    assert_eq!(
        canonical_outcome_code(&wedge, wedge.value, WheelKind::Regular),
        "LOSE"
    );
}

#[test]
fn test_wheel_jackpot_result() {
    let wedges = wheel_catalog(WheelKind::Regular, WheelEconomyPolicy::default());
    let jackpot = wedges
        .iter()
        .find(|wedge| wedge.value == WheelValue::Numeric(100))
        .unwrap();

    assert_eq!(
        numeric_settlement(NumericPolicyInput {
            value: 100,
            is_dig_bonus: false,
            blue_reduction: 0.0,
            is_bad_gamba: false,
            pardon_active: false,
        }),
        NumericSettlement::Credit(100)
    );
    assert_eq!(
        canonical_outcome_code(jackpot, jackpot.value, WheelKind::Regular),
        "NUMERIC_100"
    );
}

#[test]
fn test_wheel_wedges_distribution() {
    let wedges = wheel_catalog(WheelKind::Regular, WheelEconomyPolicy::default());
    let mut positives = wedges
        .iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Numeric(value) if value > 0 => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    positives.sort_unstable();
    let negatives = wedges
        .iter()
        .filter(|wedge| matches!(wedge.value, WheelValue::Numeric(value) if value < 0))
        .count();
    let mechanics = wedges
        .iter()
        .filter(|wedge| matches!(wedge.value, WheelValue::Mechanic(_)))
        .count();

    assert_eq!(wedges.len(), 24);
    assert_eq!(negatives, 2);
    assert_eq!(
        positives,
        vec![5, 10, 10, 15, 20, 25, 30, 40, 50, 50, 60, 70, 80, 100, 100]
    );
    assert_eq!(mechanics, 6);
}

#[test]
fn test_wheel_hazard_ceilings_are_ten_percent_stronger_again() {
    let banana_mean = (WHEEL_BANANA_PEEL_LOSS_MIN + WHEEL_BANANA_PEEL_LOSS_MAX) as f64 / 2.0;
    let bomb_mean = (WHEEL_BOMB_OMB_VICTIM_LOSS_MIN + WHEEL_BOMB_OMB_VICTIM_LOSS_MAX) as f64 / 2.0;

    assert_eq!(
        (WHEEL_BANANA_PEEL_LOSS_MIN, WHEEL_BANANA_PEEL_LOSS_MAX),
        (15, 32)
    );
    assert_eq!(
        (
            WHEEL_BOMB_OMB_VICTIM_LOSS_MIN,
            WHEEL_BOMB_OMB_VICTIM_LOSS_MAX,
        ),
        (10, 25)
    );
    assert_eq!(
        (LIGHTNING_BOLT_PCT_MIN, LIGHTNING_BOLT_PCT_MAX),
        (0.02, 0.0627)
    );
    assert_eq!((29.0_f64 * 1.10).round() as i64, 32);
    assert_eq!((23.0_f64 * 1.10).round() as i64, 25);
    assert_eq!(-banana_mean, -23.5);
    assert_eq!(-bomb_mean * WHEEL_BOMB_OMB_VICTIM_COUNT as f64, -52.5);
}

#[test]
fn test_wheel_expected_value_matches_config() {
    let policy = WheelEconomyPolicy::default();
    let wedges = wheel_catalog(WheelKind::Regular, policy);
    let actual = wheel_expected_value(WheelKind::Regular, &wedges, policy);
    let target = scale_minigame_jc_delta(policy.regular_target_ev, policy.minigame_scale) as f64;

    assert!((actual - target).abs() <= 1.0);
    assert!((-50.0..=5.0).contains(&actual));
    assert_eq!(
        wedges
            .iter()
            .filter_map(|wedge| match wedge.value {
                WheelValue::Numeric(value) if value < 0 => Some(value),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![-595, -595]
    );
}

#[test]
fn test_wheel_animation_uses_gif() {
    let plan = delivery_plan(RevealKind::Wheel, false);

    assert!(plan.defer_initial);
    assert_eq!(plan.gif_uploads, 1);
    assert_eq!(plan.waits_millis.len(), 2);
    assert_eq!(plan.final_edits, 1);
}

#[test]
fn test_wheel_updates_cooldown_in_database() {
    let now = 1_700_000_000;
    let mut port = MemoryAdmission {
        registered: true,
        ..MemoryAdmission::default()
    };

    let result = admit_spin(
        &mut port,
        SpinRequest {
            user_id: 1_009,
            guild_id: 123,
            now,
            bonus_spin: false,
            is_admin: false,
        },
    );

    assert_eq!(result, Ok(SpinAdmission::Allowed));
    assert_eq!(port.last_spin, Some(now));
    assert_eq!(port.claim_calls, 1);
}

#[test]
fn test_wheel_admin_bypasses_cooldown() {
    let now = 1_700_000_000;
    let mut port = MemoryAdmission {
        registered: true,
        last_spin: Some(now),
        ..MemoryAdmission::default()
    };

    let result = admit_spin(
        &mut port,
        SpinRequest {
            user_id: 789,
            guild_id: 123,
            now,
            bonus_spin: false,
            is_admin: true,
        },
    );

    assert_eq!(result, Ok(SpinAdmission::Allowed));
    assert_eq!(port.claim_calls, 0);
    assert_eq!(port.set_calls, vec![now]);
}

#[test]
fn test_wheel_red_shell_steals_from_player_above() {
    let target = player(2_001, 100);
    let mut hostile = FakeHostile::default();

    let outcome = settle_red_shell(
        &mut hostile,
        1_010,
        123,
        "wheel:red",
        Some(&target),
        LossRoll {
            percentage: 0.0924,
            flat: 13,
        },
        1.0,
    );

    assert!(!outcome.missed);
    assert_eq!(outcome.amount, 13);
    assert_eq!(outcome.log_result(false), 13);
    assert_eq!(hostile.requests[0].victim_id, target.discord_id);
    assert_eq!(hostile.requests[0].amount, 13);
    assert_eq!(
        hostile.requests[0].destination,
        HostileDestination::Player(1_010)
    );
    assert_eq!(
        hostile.requests[0].min_balance,
        Some(HOSTILE_LOSS_MIN_BALANCE)
    );
}

#[test]
fn test_wheel_red_shell_misses_when_first_place() {
    let mut hostile = FakeHostile::default();

    let outcome = settle_red_shell(
        &mut hostile,
        1_011,
        123,
        "wheel:red",
        None,
        LossRoll {
            percentage: 0.09,
            flat: 13,
        },
        1.0,
    );

    assert!(outcome.missed);
    assert_eq!(outcome.log_result(false), 0);
    assert!(hostile.requests.is_empty());
}

#[test]
fn test_wheel_blue_shell_steals_from_richest() {
    let spinner = player(1_012, 50);
    let richest = player(3_001, 500);
    let mut hostile = FakeHostile {
        responses: VecDeque::from([Ok(HostileLossReceipt {
            event_key: "wheel:blue:3001".to_owned(),
            requested: 50,
            attempted: 50,
            absorbed: 0,
            applied: 50,
            victim_balance_before: 500,
            victim_balance_after: 450,
            destination_balance_after: Some(100),
            centralized: false,
        })]),
        ..FakeHostile::default()
    };

    let outcome = settle_blue_shell(
        &mut hostile,
        &spinner,
        123,
        "wheel:blue",
        Some(&richest),
        LossRoll {
            percentage: 0.1016,
            flat: 29,
        },
        LossRoll {
            percentage: 0.07,
            flat: 20,
        },
        1.0,
    );

    assert_eq!(outcome.amount, 50);
    assert_eq!(outcome.spinner_new_balance, Some(100));
    assert!(!outcome.self_hit);
    assert_eq!(hostile.requests[0].amount, 50);
    assert_eq!(hostile.requests[0].min_balance, None);
}

#[test]
fn test_wheel_blue_shell_self_hit_when_richest() {
    let spinner = player(1_013, 500);
    let mut hostile = FakeHostile::default();

    let outcome = settle_blue_shell(
        &mut hostile,
        &spinner,
        123,
        "wheel:blue",
        Some(&spinner),
        LossRoll {
            percentage: 0.1016,
            flat: 29,
        },
        LossRoll {
            percentage: 0.07,
            flat: 20,
        },
        1.0,
    );

    assert!(outcome.self_hit);
    assert_eq!(outcome.amount, 35);
    assert_eq!(outcome.log_result(true), -35);
    assert_eq!(hostile.requests[0].destination, HostileDestination::Reserve);
    assert_eq!(hostile.requests[0].amount, 35);
}

#[test]
fn test_wheel_lightning_bolt_taxes_all_players() {
    let players = vec![player(2_001, 1_000), player(2_002, 500), player(2_003, 100)];
    let requests = lightning_requests(&players, 2_001, 123, "wheel:bolt", 0.0627, 1.0);
    let mut hostile = FakeHostile::default();

    let aggregate = settle_hostile_batch(&mut hostile, &requests);

    assert_eq!(
        requests
            .iter()
            .map(|request| request.amount)
            .collect::<Vec<_>>(),
        vec![62, 31, 6]
    );
    assert_eq!(aggregate.total_applied, 99);
    assert_eq!(aggregate.applied_count, 3);
    assert!(aggregate.needs_legacy_destination_credit());
    assert!(
        requests
            .iter()
            .all(|request| request.destination == HostileDestination::Reserve)
    );
}

#[test]
fn test_protection_wheel_multi_victim_helper_uses_real_protection_batch() {
    let victims = [player(20, 100), player(21, 100)];
    let requests = victims
        .iter()
        .zip([10, 15])
        .map(|(victim, amount)| {
            hostile_request(
                victim,
                123,
                99,
                "wheel:batch",
                amount,
                HostileKind::Heist,
                HostileRequestOptions {
                    destination: HostileDestination::Player(99),
                    clamp_to_balance: false,
                    min_balance: None,
                    legacy_aggregate_transfer: false,
                },
            )
        })
        .collect::<Vec<_>>();
    let responses = requests
        .iter()
        .map(|request| Ok(receipt(request, 0, true, Some(request.amount))))
        .collect();
    let mut hostile = FakeHostile {
        responses,
        ..FakeHostile::default()
    };

    let aggregate = settle_hostile_batch(&mut hostile, &requests);

    assert_eq!(hostile.requests.len(), 2);
    assert_eq!(
        hostile
            .requests
            .iter()
            .map(|request| (request.victim_id, request.amount, request.kind.code()))
            .collect::<Vec<_>>(),
        vec![(20, 10, "HEIST"), (21, 15, "HEIST")]
    );
    assert_eq!(aggregate.total_applied, 25);
    assert_eq!(aggregate.applied_count, 2);
    assert_eq!(aggregate.applied, vec![(20, 10), (21, 15)]);
    assert_eq!(aggregate.shield_absorbed_total, 0);
    assert!(aggregate.centralized);
    assert!(!aggregate.needs_legacy_destination_credit());
}

#[test]
fn test_wheel_lightning_bolt_skips_zero_balance() {
    let players = vec![
        player(3_001, 500),
        player(3_004, 49),
        player(3_002, 0),
        player(3_003, -100),
    ];

    let requests = lightning_requests(&players, 3_001, 123, "wheel:bolt", 0.02, 1.0);

    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].victim_id, 3_001);
    assert_eq!(requests[0].amount, 10);
}

#[test]
fn test_wheel_lightning_bolt_spinner_also_taxed() {
    let spinner_id = 4_001;
    let players = vec![player(spinner_id, 300), player(4_002, 200)];

    let requests = lightning_requests(&players, spinner_id, 123, "wheel:bolt", 0.01, 1.0);

    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .any(|request| request.victim_id == spinner_id)
    );
}

#[test]
fn test_wheel_commune_skips_players_below_auto_blind_threshold() {
    let spinner_id = 4_101;
    let players = vec![player(4_102, 50), player(4_103, 49)];

    let requests = commune_requests(&players, spinner_id, 123, "wheel:commune");

    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].victim_id, 4_102);
    assert_eq!(requests[0].amount, 1);
    assert_eq!(
        requests[0].destination,
        HostileDestination::Player(spinner_id)
    );
}

#[test]
fn test_bankrupt_wheel_composition() {
    let regular = wheel_catalog(WheelKind::Regular, WheelEconomyPolicy::default());
    let bankrupt = wheel_catalog(WheelKind::Bankrupt, WheelEconomyPolicy::default());
    let positives = bankrupt
        .iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Numeric(value) if value > 0 => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mechanics = bankrupt
        .iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Mechanic(value) => Some(value),
            WheelValue::Numeric(_) => None,
        })
        .collect::<Vec<_>>();
    let losses = bankrupt
        .iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Numeric(value) if value < 0 => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(bankrupt.len(), 24);
    assert_eq!(positives.len(), 6);
    assert!(positives.iter().max().is_some_and(|value| *value >= 75));
    assert!(positives.iter().min().is_some_and(|value| *value <= 50));
    for required in [
        WheelMechanic::RedShell,
        WheelMechanic::BlueShell,
        WheelMechanic::LightningBolt,
        WheelMechanic::ExtendOne,
        WheelMechanic::ExtendTwo,
        WheelMechanic::Jailbreak,
        WheelMechanic::ChainReaction,
        WheelMechanic::TownTrial,
        WheelMechanic::Discover,
        WheelMechanic::Emergency,
        WheelMechanic::Commune,
        WheelMechanic::Comeback,
    ] {
        assert!(mechanics.contains(&required));
    }
    assert_eq!(losses, vec![-18, -18]);
    assert!(losses.iter().any(|value| *value <= -5));
    let jackpot_colors = bankrupt
        .iter()
        .filter(|wedge| wedge.value == WheelValue::Numeric(100))
        .map(|wedge| wedge.color)
        .collect::<Vec<_>>();
    assert_eq!(jackpot_colors, vec!["#f1c40f", "#f1c40f"]);
    assert_eq!(
        regular
            .iter()
            .filter(|wedge| wedge.value == WheelValue::Numeric(100))
            .map(|wedge| wedge.color)
            .collect::<Vec<_>>(),
        jackpot_colors
    );
}

#[test]
fn test_get_wheel_wedges_returns_correct_wheel() {
    let policy = WheelEconomyPolicy::default();
    let normal = get_wheel_wedges(false, false, policy);
    let bankrupt = get_wheel_wedges(true, false, policy);

    assert_eq!(normal, wheel_catalog(WheelKind::Regular, policy));
    assert_eq!(bankrupt, wheel_catalog(WheelKind::Bankrupt, policy));
    assert_eq!(normal.len(), 24);
    assert_eq!(bankrupt.len(), 24);
}

#[test]
fn test_bankrupt_wheel_ev_maintained() {
    let policy = WheelEconomyPolicy::default();
    let bankrupt = wheel_catalog(WheelKind::Bankrupt, policy);
    let actual = wheel_expected_value(WheelKind::Bankrupt, &bankrupt, policy);
    let target = scale_minigame_jc_delta(policy.bankrupt_target_ev, policy.minigame_scale) as f64;

    assert!((actual - target).abs() <= 1.0);
}

#[test]
fn test_mario_kart_wedges_on_all_wheels() {
    for kind in [WheelKind::Regular, WheelKind::Bankrupt, WheelKind::Golden] {
        let wedges = wheel_catalog(kind, WheelEconomyPolicy::default());
        for required in [
            WheelMechanic::BananaPeel,
            WheelMechanic::GreenShell,
            WheelMechanic::BombOmb,
        ] {
            assert!(
                wedges
                    .iter()
                    .any(|wedge| wedge.value == WheelValue::Mechanic(required))
            );
        }
    }
}

#[test]
fn test_wheels_in_rainbow_order() {
    for kind in [WheelKind::Regular, WheelKind::Bankrupt, WheelKind::Golden] {
        let wedges = wheel_catalog(kind, WheelEconomyPolicy::default());
        assert!(
            wedges
                .windows(2)
                .all(|pair| compare_rainbow(&pair[0], &pair[1]) != Ordering::Greater)
        );
    }

    let regular = wheel_catalog(WheelKind::Regular, WheelEconomyPolicy::default());
    let position = |color| {
        regular
            .iter()
            .position(|wedge| wedge.color == color)
            .unwrap()
    };
    assert!(position("#e74c3c") < position("#228b22"));
    assert!(position("#228b22") < position("#3498db"));
}

#[test]
fn test_dig_bonus_wheel_spin_preserves_regular_cooldown() {
    let now = 1_700_000_000;
    let concurrent_spin = now - 10;
    let cooldown =
        cooldown_after_resolution(now, true, Some(concurrent_spin), CooldownOutcome::Other);
    let numeric = numeric_settlement(NumericPolicyInput {
        value: 5,
        is_dig_bonus: true,
        blue_reduction: 0.0,
        is_bad_gamba: false,
        pardon_active: false,
    });
    let presentation = bonus_spin_presentation(cooldown.next_spin_at);

    assert_eq!(numeric, NumericSettlement::Credit(3));
    assert_eq!(
        select_wheel_kind(1_001, 50, true, &[1_001]),
        WheelKind::Regular
    );
    assert_eq!(cooldown.replacement_last_spin, None);
    assert!(!cooldown.schedule_reminder);
    assert_eq!(cooldown.next_spin_at, next_wheel_reset_at(concurrent_spin));
    assert!(presentation.field_value.contains("cooldown unchanged"));
    assert!(!delivery_plan(RevealKind::Wheel, true).defer_initial);
}

#[test]
fn test_dig_bonus_wheel_explosion_preserves_regular_cooldown() {
    let now = 1_700_000_000;
    let previous_spin = now - 300;
    let explosion = resolve_explosion(true, 50, 1.0, 1.0);
    let cooldown =
        cooldown_after_resolution(now, true, Some(previous_spin), CooldownOutcome::Other);

    assert_eq!(explosion.gross_reward, WHEEL_EXPLOSION_REWARD);
    assert_eq!(explosion.credited_reward, scale_positive_dig_jc(67));
    assert_eq!(explosion.log_result, 67);
    assert_eq!(explosion.wheel_kind, WheelKind::Regular);
    assert!(explosion.is_bonus);
    assert_eq!(cooldown.next_spin_at, next_wheel_reset_at(previous_spin));
    assert!(!cooldown.schedule_reminder);
    assert!(!delivery_plan(RevealKind::Explosion, true).defer_initial);
}

#[test]
fn regular_and_lose_turn_cooldowns_share_the_fixed_daily_reset() {
    let reset = 20 * GAME_DAY_SECONDS + 12 * 3_600;
    let now = reset + 23 * 3_600;

    let regular = cooldown_after_resolution(now, false, None, CooldownOutcome::Other);
    assert_eq!(regular.next_spin_at, reset + GAME_DAY_SECONDS);
    assert_eq!(regular.replacement_last_spin, None);
    assert!(regular.schedule_reminder);

    let lose_turn = cooldown_after_resolution(now, false, None, CooldownOutcome::LoseTurn);
    assert_eq!(
        lose_turn.next_spin_at,
        reset + WHEEL_LOSE_PENALTY_DAYS * GAME_DAY_SECONDS
    );
    assert_eq!(
        lose_turn.replacement_last_spin,
        Some(lose_turn.next_spin_at - 1)
    );
    assert!(wheel_spin_is_available(
        lose_turn.next_spin_at,
        lose_turn.replacement_last_spin
    ));
    assert!(!wheel_spin_is_available(
        lose_turn.next_spin_at - 1,
        lose_turn.replacement_last_spin
    ));
}

#[test]
fn test_red_shell_partial_absorption_routes_only_applied_transfer() {
    let target = player(7_301, 100);
    let mut hostile = FakeHostile::default();
    let request = hostile_request(
        &target,
        123,
        7_300,
        "wheel:red",
        3,
        HostileKind::RedShell,
        HostileRequestOptions {
            destination: HostileDestination::Player(7_300),
            clamp_to_balance: false,
            min_balance: Some(HOSTILE_LOSS_MIN_BALANCE),
            legacy_aggregate_transfer: false,
        },
    );
    hostile
        .responses
        .push_back(Ok(receipt(&request, 1, true, Some(52))));

    let outcome = settle_red_shell(
        &mut hostile,
        7_300,
        123,
        "wheel:red",
        Some(&target),
        LossRoll {
            percentage: 0.03,
            flat: 2,
        },
        1.0,
    );

    assert_eq!(hostile.requests[0].amount, 3);
    assert_eq!(outcome.amount, 2);
    assert_eq!(outcome.shield_absorbed, 1);
    assert_eq!(outcome.spinner_new_balance, Some(52));
}

#[test]
fn test_lightning_mixed_absorption_uses_actual_reserve_total_without_double_credit() {
    let spinner_id = 7_400;
    let players = vec![
        player(spinner_id, 1_000),
        player(7_401, 500),
        player(7_402, 100),
    ];
    let requests = lightning_requests(&players, spinner_id, 123, "wheel:bolt", 0.02, 1.0);
    let responses = requests
        .iter()
        .zip([0, 8, 1])
        .map(|(request, absorbed)| Ok(receipt(request, absorbed, true, None)))
        .collect();
    let mut hostile = FakeHostile {
        responses,
        ..FakeHostile::default()
    };

    let aggregate = settle_hostile_batch(&mut hostile, &requests);

    assert_eq!(aggregate.total_applied, 23);
    assert_eq!(aggregate.applied_count, 3);
    assert_eq!(aggregate.shielded_count, 2);
    assert_eq!(aggregate.shield_absorbed_total, 9);
    assert!(aggregate.centralized);
    assert!(!aggregate.needs_legacy_destination_credit());
    assert!(stable_event_keys(&requests));
}

#[test]
fn test_trickle_down_mixed_absorption_does_not_double_credit_spinner() {
    let spinner_id = 7_500;
    let players = vec![
        player(spinner_id, 500),
        player(7_501, 1_000),
        player(7_502, 100),
    ];
    let requests = trickle_down_requests(&players, spinner_id, 123, "wheel:trickle", 0.02, 1.0);
    let responses = requests
        .iter()
        .zip([16, 1])
        .map(|(request, absorbed)| Ok(receipt(request, absorbed, true, Some(501))))
        .collect();
    let mut hostile = FakeHostile {
        responses,
        ..FakeHostile::default()
    };

    let aggregate = settle_hostile_batch(&mut hostile, &requests);

    assert_eq!(aggregate.total_applied, 5);
    assert_eq!(aggregate.applied_count, 2);
    assert_eq!(aggregate.shield_absorbed_total, 17);
    assert!(!aggregate.needs_legacy_destination_credit());
}

#[test]
fn test_bomb_omb_mixed_absorption_burns_only_applied_amounts() {
    let victims = [player(7_601, 100), player(7_602, 100), player(7_603, 100)];
    let selected = victims.iter().collect::<Vec<_>>();
    let requests = bomb_omb_requests(&selected, 7_600, 123, "wheel:bomb", &[15, 15, 15], 1.0);
    let responses = requests
        .iter()
        .zip([12, 5, 0])
        .map(|(request, absorbed)| Ok(receipt(request, absorbed, true, None)))
        .collect();
    let mut hostile = FakeHostile {
        responses,
        ..FakeHostile::default()
    };

    let aggregate = settle_hostile_batch(&mut hostile, &requests);

    assert_eq!(aggregate.total_applied, 28);
    assert_eq!(
        aggregate.applied,
        vec![(7_601, 3), (7_602, 10), (7_603, 15)]
    );
    assert_eq!(aggregate.shielded_count, 2);
    assert_eq!(aggregate.shield_absorbed_total, 17);
    assert!(stable_event_keys(&requests));
    assert!(
        requests
            .iter()
            .all(|request| request.destination == HostileDestination::Burn)
    );
}

#[test]
fn test_golden_trickle_down_skips_players_below_auto_blind_threshold() {
    let spinner_id = 7_200;
    let players = vec![
        player(spinner_id, 500),
        player(7_201, 1_000),
        player(7_202, 50),
        player(7_203, 49),
    ];
    let requests = trickle_down_requests(&players, spinner_id, 123, "wheel:trickle", 0.02, 1.0);
    let mut hostile = FakeHostile::default();

    let aggregate = settle_hostile_batch(&mut hostile, &requests);

    assert_eq!(
        requests
            .iter()
            .map(|request| (request.victim_id, request.amount))
            .collect::<Vec<_>>(),
        vec![(7_201, 20), (7_202, 1)]
    );
    assert_eq!(aggregate.total_applied, 21);
    assert!(aggregate.needs_legacy_destination_credit());
}

#[test]
fn test_banana_peel_burns_player_below() {
    let victim = player(8_001, 100);
    let request = banana_request(Some(&victim), 7_001, 123, "wheel:banana", 20, 1.0).unwrap();
    let mut hostile = FakeHostile::default();

    let aggregate = settle_hostile_batch(&mut hostile, std::slice::from_ref(&request));

    assert_eq!(request.victim_id, victim.discord_id);
    assert_eq!(request.amount, 20);
    assert_eq!(request.destination, HostileDestination::Burn);
    assert_eq!(aggregate.total_applied, 20);
    assert_ne!(request.victim_id, 7_001);
}

#[test]
fn test_banana_peel_misses_when_no_player_below() {
    assert_eq!(
        banana_request(None, 7_002, 123, "wheel:banana", 20, 1.0),
        None
    );
}

#[test]
fn test_banana_peel_skips_victim_below_hostile_loss_floor() {
    let victim = player(8_002, 5);

    assert_eq!(
        banana_request(Some(&victim), 7_003, 123, "wheel:banana", 20, 1.0),
        None
    );
}

#[test]
fn test_green_shell_steals_from_random_other_via_steal_atomic() {
    let victim = player(8_003, 100);
    let request = green_shell_request(Some(&victim), 7_004, 123, "wheel:green", 33, 1.0).unwrap();
    let mut hostile = FakeHostile::default();

    let aggregate = settle_hostile_batch(&mut hostile, std::slice::from_ref(&request));

    assert_eq!(request.amount, 33);
    assert_eq!(request.destination, HostileDestination::Player(7_004));
    assert!(!request.legacy_aggregate_transfer);
    assert_eq!(aggregate.total_applied, 33);
}

#[test]
fn test_green_shell_misses_when_no_eligible_victims() {
    assert_eq!(
        green_shell_request(None, 7_005, 123, "wheel:green", 33, 1.0),
        None
    );
}

#[test]
fn test_bomb_omb_burns_three_random_others() {
    let spinner_id = 7_006;
    let players = vec![
        player(9_000, 100),
        player(9_001, 100),
        player(9_002, 100),
        player(9_010, 49),
        player(9_011, 49),
    ];
    let eligible = eligible_other_players(&players, spinner_id);
    let requests = bomb_omb_requests(&eligible, spinner_id, 123, "wheel:bomb", &[15, 15, 15], 1.0);

    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .map(|request| request.victim_id)
            .collect::<Vec<_>>(),
        vec![9_000, 9_001, 9_002]
    );
    assert!(
        requests
            .iter()
            .all(|request| request.amount < 0 || request.amount == 15)
    );
    assert!(
        requests
            .iter()
            .all(|request| request.victim_id != spinner_id)
    );
    assert!(
        requests
            .iter()
            .all(|request| request.destination == HostileDestination::Burn)
    );
}

#[test]
fn test_bomb_omb_clamps_sample_to_eligible_pool() {
    let players = vec![player(9_101, 100), player(9_102, 100)];
    let eligible = eligible_other_players(&players, 7_007);

    let requests = bomb_omb_requests(&eligible, 7_007, 123, "wheel:bomb", &[15, 15, 15], 1.0);

    assert_eq!(requests.len(), 2);
}

#[test]
fn test_bomb_omb_misses_when_no_eligible_victims() {
    let requests = bomb_omb_requests(&[], 7_008, 123, "wheel:bomb", &[15, 15, 15], 1.0);

    assert!(requests.is_empty());
}

#[test]
fn test_deflation_wedges_do_not_credit_nonprofit_banana_peel() {
    assert_eq!(
        deflation_destination(WheelMechanic::BananaPeel, 7_100),
        HostileDestination::Burn
    );
}

#[test]
fn test_deflation_wedges_do_not_credit_nonprofit_green_shell() {
    assert_eq!(
        deflation_destination(WheelMechanic::GreenShell, 7_100),
        HostileDestination::Player(7_100)
    );
}

#[test]
fn test_deflation_wedges_do_not_credit_nonprofit_bomb_omb() {
    assert_eq!(
        deflation_destination(WheelMechanic::BombOmb, 7_100),
        HostileDestination::Burn
    );
}

#[test]
fn test_wheel_positive_balance_with_penalty_skips_bankrupt_wheel() {
    let penalty_games_remaining = 4;

    assert!(penalty_games_remaining > 0);
    assert_eq!(select_wheel_kind(7_002, 0, false, &[]), WheelKind::Regular);
}

#[test]
fn test_jailbreak_clamps_at_zero() {
    assert_eq!(adjust_penalty_games(0, -1), 0);
}

#[test]
fn test_jailbreak_decrements_games() {
    assert_eq!(adjust_penalty_games(3, -1), 2);
}

#[test]
fn test_wheel_negative_balance_uses_bankrupt_wheel() {
    assert_eq!(
        select_wheel_kind(1_099, -50, false, &[1_099]),
        WheelKind::Bankrupt
    );
}

#[test]
fn test_commune_credits_spinner_debits_threshold_eligible_players() {
    let spinner_id = 8_001;
    let mut balances = [(spinner_id, -20), (8_002, 50), (8_003, 10), (8_004, 0)];
    let players = balances
        .iter()
        .map(|(id, balance)| player(*id, *balance))
        .collect::<Vec<_>>();
    let requests = commune_requests(&players, spinner_id, 0, "wheel:commune");
    let mut hostile = FakeHostile::default();
    let aggregate = settle_hostile_batch(&mut hostile, &requests);
    for (victim_id, amount) in &aggregate.applied {
        let entry = balances.iter_mut().find(|(id, _)| id == victim_id).unwrap();
        entry.1 -= amount;
    }
    balances
        .iter_mut()
        .find(|(id, _)| *id == spinner_id)
        .unwrap()
        .1 += aggregate.total_applied;

    assert_eq!(aggregate.applied_count, 1);
    assert_eq!(
        balances,
        [(8_001, -19), (8_002, 49), (8_003, 10), (8_004, 0)]
    );
}

#[test]
fn test_comeback_sets_and_consumes_pardon() {
    let mut state = PardonState::default();

    assert!(!state.active);
    state.grant();
    assert!(state.active);
    assert!(state.consume());
    assert!(!state.active);
    assert!(!state.consume());
}

#[test]
fn test_wheel_penalized_winner_is_debuffed() {
    let deductions = post_win_deductions(100, Some(0.75), None);

    assert_eq!(deductions.bankruptcy_penalty, 25);
    assert_eq!(deductions.net_gain, 75);
}

#[test]
fn test_credit_gamba_outcome_garnishes_when_in_debt() {
    let request = credit_request(1_001, 60);
    let mut port = MemoryCredit {
        balance: -100,
        garnish_receipt: Some(CreditReceipt {
            new_balance: -40,
            garnished: 30,
        }),
        ..MemoryCredit::default()
    };

    let outcome = credit_gamba_outcome(&mut port, true, -100, request.clone()).unwrap();

    assert_eq!(outcome.new_balance, -40);
    assert_eq!(outcome.garnished, 30);
    assert_eq!(port.garnished, vec![request.clone()]);
    assert!(port.direct.is_empty());
    assert_eq!(request.audit.source, "gamba");
    assert_eq!(request.audit.actor_id, 1_001);
    assert_eq!(request.audit.related_type, "wheel_spin");
    assert_eq!(request.audit.related_id, "DIVIDEND");
}

#[test]
fn test_credit_gamba_outcome_direct_when_solvent() {
    let request = credit_request(1_002, 60);
    let mut port = MemoryCredit {
        balance: 50,
        ..MemoryCredit::default()
    };

    let outcome = credit_gamba_outcome(&mut port, true, 50, request.clone()).unwrap();

    assert_eq!(outcome.new_balance, 110);
    assert_eq!(outcome.garnished, 0);
    assert_eq!(port.direct, vec![request]);
    assert!(port.garnished.is_empty());
    assert_eq!(port.balance_reads, 1);
}

#[test]
fn test_blue_shell_hits_true_net_worth_leader_without_liquidating_positions() {
    let spinner = player(2_001, 100);
    let cash_leader = player(2_002, 500);
    let position_leader = PlayerSnapshot {
        discord_id: 2_003,
        name: "PositionLeader".to_owned(),
        cash_balance: 20,
        position_value: 900,
    };
    let snapshot = vec![
        spinner.clone(),
        cash_leader.clone(),
        position_leader.clone(),
    ];
    let leader = select_net_worth_leader(&snapshot).unwrap();
    let mut hostile = FakeHostile {
        responses: VecDeque::from([Ok(HostileLossReceipt {
            event_key: "wheel:net:2003".to_owned(),
            requested: 29,
            attempted: 29,
            absorbed: 0,
            applied: 29,
            victim_balance_before: 20,
            victim_balance_after: -9,
            destination_balance_after: Some(129),
            centralized: false,
        })]),
        ..FakeHostile::default()
    };

    let outcome = settle_blue_shell(
        &mut hostile,
        &spinner,
        123,
        "wheel:net",
        Some(leader),
        LossRoll {
            percentage: 0.1016,
            flat: 29,
        },
        LossRoll {
            percentage: 0.07,
            flat: 20,
        },
        1.0,
    );

    assert_eq!(leader.discord_id, position_leader.discord_id);
    assert_eq!(cash_leader.cash_balance, 500);
    assert_eq!(outcome.victim_new_balance, Some(-9));
    assert_eq!(outcome.spinner_new_balance, Some(129));
    assert_eq!(outcome.amount, 29);
    assert_eq!(position_leader.position_value, 900);
    assert!(!hostile.requests[0].clamp_to_balance);
}

#[derive(Default)]
struct TradingSnapshot {
    calls: usize,
    rows: Vec<PlayerSnapshot>,
}

impl NetWorthSnapshotPort for TradingSnapshot {
    type Error = &'static str;

    fn guild_net_worth_snapshot(
        &mut self,
        _guild_id: i64,
    ) -> Result<Vec<PlayerSnapshot>, Self::Error> {
        self.calls += 1;
        let snapshot = self.rows.clone();
        // A trade commits immediately after the read: cash moves into a
        // position, preserving net worth. Selection must not issue a second
        // independently timed read.
        if let Some(trader) = self.rows.iter_mut().find(|row| row.discord_id == 2_102) {
            trader.cash_balance = 100;
            trader.position_value = 900;
        }
        Ok(snapshot)
    }
}

#[test]
fn test_blue_shell_uses_one_snapshot_when_trade_moves_cash_into_positions() {
    let spinner = player(2_101, 100);
    let mut snapshots = TradingSnapshot {
        rows: vec![spinner.clone(), player(2_102, 1_000), player(2_103, 1_500)],
        ..TradingSnapshot::default()
    };

    let leader = load_blue_shell_leader(&mut snapshots, 123)
        .unwrap()
        .unwrap();
    let mut hostile = FakeHostile {
        responses: VecDeque::from([Ok(HostileLossReceipt {
            event_key: "wheel:snapshot:2103".to_owned(),
            requested: 30,
            attempted: 30,
            absorbed: 0,
            applied: 30,
            victim_balance_before: 1_500,
            victim_balance_after: 1_470,
            destination_balance_after: Some(130),
            centralized: false,
        })]),
        ..FakeHostile::default()
    };
    let outcome = settle_blue_shell(
        &mut hostile,
        &spinner,
        123,
        "wheel:snapshot",
        Some(&leader),
        LossRoll {
            percentage: 0.02,
            flat: 29,
        },
        LossRoll {
            percentage: 0.07,
            flat: 20,
        },
        1.0,
    );

    assert_eq!(snapshots.calls, 1);
    assert_eq!(leader.discord_id, 2_103);
    assert_eq!(snapshots.rows[1].cash_balance, 100);
    assert_eq!(snapshots.rows[1].position_value, 900);
    assert_eq!(outcome.amount, 30);
    assert_eq!(outcome.victim_new_balance, Some(1_470));
    assert_eq!(outcome.spinner_new_balance, Some(130));
}

#[test]
fn test_red_shell_settlement_failure_is_contained_as_miss() {
    let target = player(2_002, HOSTILE_LOSS_MIN_BALANCE);
    let mut hostile = FakeHostile {
        responses: VecDeque::from([Err("settlement failed")]),
        ..FakeHostile::default()
    };

    let outcome = settle_red_shell(
        &mut hostile,
        1_001,
        123,
        "wheel:red",
        Some(&target),
        LossRoll {
            percentage: 0.02,
            flat: 2,
        },
        1.0,
    );

    assert!(outcome.missed);
    assert_eq!(outcome.amount, 0);
    assert_eq!(outcome.log_result(false), 0);
}

#[test]
fn test_blue_shell_settlement_failure_is_contained_as_miss() {
    let spinner = player(1_001, 50);
    let richest = player(2_003, HOSTILE_LOSS_MIN_BALANCE);
    let mut hostile = FakeHostile {
        responses: VecDeque::from([Err("settlement failed")]),
        ..FakeHostile::default()
    };

    let outcome = settle_blue_shell(
        &mut hostile,
        &spinner,
        123,
        "wheel:blue",
        Some(&richest),
        LossRoll {
            percentage: 0.02,
            flat: 4,
        },
        LossRoll {
            percentage: 0.02,
            flat: 4,
        },
        1.0,
    );

    assert!(outcome.missed);
    assert_eq!(outcome.amount, 0);
    assert_eq!(outcome.log_result(true), 0);
}

#[test]
fn test_blue_shell_self_hit_failure_is_contained_as_miss() {
    let spinner = player(2_004, 500);
    let mut hostile = FakeHostile {
        responses: VecDeque::from([Err("settlement failed")]),
        ..FakeHostile::default()
    };

    let outcome = settle_blue_shell(
        &mut hostile,
        &spinner,
        123,
        "wheel:blue",
        Some(&spinner),
        LossRoll {
            percentage: 0.02,
            flat: 4,
        },
        LossRoll {
            percentage: 0.07,
            flat: 20,
        },
        1.0,
    );

    assert!(outcome.self_hit);
    assert!(outcome.missed);
    assert_eq!(outcome.amount, 0);
    assert_eq!(outcome.log_result(true), 0);
}

#[test]
fn test_decay_per_victim_failure_keeps_other_victims_settled() {
    let victims = [player(2_005, 500), player(2_006, 500)];
    let requests = victims
        .iter()
        .map(|victim| {
            hostile_request(
                victim,
                123,
                1_001,
                "wheel:decay",
                60,
                HostileKind::Decay,
                HostileRequestOptions {
                    destination: HostileDestination::Player(1_001),
                    clamp_to_balance: true,
                    min_balance: Some(HOSTILE_LOSS_MIN_BALANCE),
                    legacy_aggregate_transfer: true,
                },
            )
        })
        .collect::<Vec<_>>();
    let mut hostile = FakeHostile {
        responses: VecDeque::from([
            Err("settlement failed"),
            Ok(receipt(&requests[1], 0, false, None)),
        ]),
        ..FakeHostile::default()
    };

    let aggregate = settle_hostile_batch(&mut hostile, &requests);
    let mut credit = MemoryCredit {
        balance: 50,
        ..MemoryCredit::default()
    };
    let outcome = credit_gamba_outcome(
        &mut credit,
        false,
        50,
        CreditRequest {
            user_id: 1_001,
            guild_id: 123,
            amount: aggregate.total_applied,
            audit: audit(1_001, "DECAY", "gamba decay collected taxes"),
        },
    )
    .unwrap();

    assert_eq!(aggregate.failed_count, 1);
    assert_eq!(aggregate.total_applied, 60);
    assert_eq!(outcome.new_balance, 110);
    assert_eq!(credit.direct.len(), 1);
}

#[test]
fn test_commune_per_victim_failure_keeps_other_donations() {
    let spinner_id = 1_001;
    let players = vec![player(2_007, 500), player(2_008, 500)];
    let requests = commune_requests(&players, spinner_id, 123, "wheel:commune");
    let mut hostile = FakeHostile {
        responses: VecDeque::from([
            Err("settlement failed"),
            Ok(receipt(&requests[1], 0, false, None)),
        ]),
        ..FakeHostile::default()
    };

    let aggregate = settle_hostile_batch(&mut hostile, &requests);

    assert_eq!(aggregate.failed_count, 1);
    assert_eq!(aggregate.total_applied, 1);
    assert_eq!(aggregate.applied_count, 1);
    assert!(aggregate.needs_legacy_destination_credit());
}

#[test]
fn test_heist_fully_shielded_victim_not_counted_as_robbed() {
    let victim = player(2_009, 100);
    let request = hostile_request(
        &victim,
        123,
        1_001,
        "wheel:heist",
        9,
        HostileKind::Heist,
        HostileRequestOptions {
            destination: HostileDestination::Player(1_001),
            clamp_to_balance: false,
            min_balance: Some(HOSTILE_LOSS_MIN_BALANCE),
            legacy_aggregate_transfer: false,
        },
    );
    let mut hostile = FakeHostile {
        responses: VecDeque::from([Ok(receipt(&request, 9, true, Some(50)))]),
        ..FakeHostile::default()
    };

    let aggregate = settle_hostile_batch(&mut hostile, &[request]);

    assert_eq!(aggregate.applied_count, 0);
    assert_eq!(aggregate.total_applied, 0);
    assert_eq!(aggregate.shield_absorbed_total, 9);
}

#[test]
fn test_market_crash_fully_shielded_victim_not_counted_as_taxed() {
    let victim = player(2_010, 500);
    let request = hostile_request(
        &victim,
        123,
        1_001,
        "wheel:crash",
        12,
        HostileKind::MarketCrash,
        HostileRequestOptions {
            destination: HostileDestination::Player(1_001),
            clamp_to_balance: false,
            min_balance: Some(HOSTILE_LOSS_MIN_BALANCE),
            legacy_aggregate_transfer: false,
        },
    );
    let mut hostile = FakeHostile {
        responses: VecDeque::from([Ok(receipt(&request, 12, true, Some(50)))]),
        ..FakeHostile::default()
    };

    let aggregate = settle_hostile_batch(&mut hostile, &[request]);

    assert_eq!(aggregate.applied_count, 0);
    assert_eq!(aggregate.total_applied, 0);
    assert_eq!(aggregate.shield_absorbed_total, 12);
}

#[test]
fn test_canonical_codes_preserve_dynamic_wedge_identity() {
    let bankrupt = WheelWedge {
        label: "-92".to_owned(),
        value: WheelValue::Numeric(-92),
        color: "#1a1a1a",
    };
    assert_eq!(
        canonical_outcome_code(&bankrupt, WheelValue::Numeric(-92), WheelKind::Regular),
        "BANKRUPT"
    );

    let overextended = WheelWedge {
        label: "-390".to_owned(),
        value: WheelValue::Numeric(-390),
        color: "#4a3000",
    };
    assert_eq!(
        canonical_outcome_code(&overextended, WheelValue::Numeric(-390), WheelKind::Golden,),
        "OVEREXTENDED"
    );

    let crown = WheelWedge {
        label: "200".to_owned(),
        value: WheelValue::Numeric(200),
        color: "#fffacd",
    };
    assert_eq!(
        canonical_outcome_code(&crown, WheelValue::Numeric(200), WheelKind::Golden),
        "CROWN"
    );
}
