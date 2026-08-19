use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use super::*;

const USER: i64 = 1;
const GUILD: i64 = 123;

#[derive(Clone)]
struct ConcurrencyTracker {
    barrier: Arc<Barrier>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl ConcurrencyTracker {
    fn new(worker_count: usize) -> Self {
        Self {
            barrier: Arc::new(Barrier::new(worker_count)),
            active: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn visit(&self) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        self.barrier.wait();
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Default)]
struct FakeHistoryPort {
    bets: Vec<BetHistoryRow>,
    predictions: Vec<PredictionHistoryRow>,
    wheel: Vec<WheelHistoryRow>,
    doubles: Vec<DoubleOrNothingHistoryRow>,
    tips: Vec<TipHistoryRow>,
    disbursements: Vec<DisbursementHistoryRow>,
    bonuses: Vec<MatchBonusHistoryRow>,
    dig: Option<Vec<DigHistoryRow>>,
    tracker: Option<ConcurrencyTracker>,
}

impl FakeHistoryPort {
    fn track(&self) {
        if let Some(tracker) = &self.tracker {
            tracker.visit();
        }
    }
}

impl BalanceHistoryPort for FakeHistoryPort {
    fn bet_history(&self, _discord_id: DiscordId, _guild_id: GuildId) -> Vec<BetHistoryRow> {
        self.track();
        self.bets.clone()
    }

    fn prediction_history(
        &self,
        _discord_id: DiscordId,
        _guild_id: GuildId,
    ) -> Vec<PredictionHistoryRow> {
        self.track();
        self.predictions.clone()
    }

    fn wheel_history(&self, _discord_id: DiscordId, _guild_id: GuildId) -> Vec<WheelHistoryRow> {
        self.track();
        self.wheel.clone()
    }

    fn double_or_nothing_history(
        &self,
        _discord_id: DiscordId,
        _guild_id: GuildId,
    ) -> Vec<DoubleOrNothingHistoryRow> {
        self.track();
        self.doubles.clone()
    }

    fn tip_history(&self, _discord_id: DiscordId, _guild_id: GuildId) -> Vec<TipHistoryRow> {
        self.track();
        self.tips.clone()
    }

    fn disbursement_history(
        &self,
        _discord_id: DiscordId,
        _guild_id: GuildId,
    ) -> Vec<DisbursementHistoryRow> {
        self.track();
        self.disbursements.clone()
    }

    fn match_bonus_history(
        &self,
        _discord_id: DiscordId,
        _guild_id: GuildId,
    ) -> Vec<MatchBonusHistoryRow> {
        self.track();
        self.bonuses.clone()
    }

    fn dig_history(
        &self,
        _discord_id: DiscordId,
        _guild_id: GuildId,
    ) -> Option<Vec<DigHistoryRow>> {
        self.track();
        self.dig.clone()
    }
}

fn service(port: FakeHistoryPort) -> BalanceHistoryService<FakeHistoryPort> {
    BalanceHistoryService::new(port)
}

fn bet(time: i64, profit: i64, match_id: i64) -> BetHistoryRow {
    BetHistoryRow {
        bet_time: time,
        profit,
        won: profit > 0,
        amount: 10,
        leverage: 1,
        match_id,
    }
}

fn dig(
    actor_id: i64,
    target_id: Option<i64>,
    action_type: &str,
    detail: Option<&str>,
    created_at: i64,
    jc_delta: i64,
) -> DigHistoryRow {
    DigHistoryRow {
        actor_id,
        target_id,
        action_type: action_type.to_owned(),
        detail: detail.map(str::to_owned),
        created_at,
        jc_delta,
    }
}

fn deltas(history: &BalanceHistory) -> Vec<i64> {
    history
        .series
        .iter()
        .map(|point| point.event.delta)
        .collect()
}

#[test]
fn test_empty_history_returns_empty_series_and_totals() {
    let history = service(FakeHistoryPort::default()).get_balance_event_series(USER, GUILD);
    assert!(history.series.is_empty());
    assert!(history.source_totals.is_empty());
    assert!(history.chart_points().is_empty());
}

#[test]
fn test_async_history_loads_all_sources_concurrently() {
    let base = FakeHistoryPort {
        bets: vec![bet(2_000, 10, 1)],
        wheel: vec![WheelHistoryRow {
            spin_time: 1_000,
            result: 20,
            outcome_metadata: None,
        }],
        dig: Some(Vec::new()),
        ..FakeHistoryPort::default()
    };
    let expected = service(base.clone()).get_balance_event_series(USER, GUILD);
    let tracker = ConcurrencyTracker::new(8);
    let mut concurrent = base;
    concurrent.tracker = Some(tracker.clone());
    let actual = service(concurrent)
        .get_balance_event_series_concurrently(USER, GUILD)
        .expect("parallel history");
    assert_eq!(actual, expected);
    assert_eq!(tracker.peak.load(Ordering::SeqCst), 8);
}

#[test]
fn test_single_source_bet_only_series() {
    let history = service(FakeHistoryPort {
        bets: vec![bet(1_000, 50, 1), bet(2_000, -10, 2)],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(history.series.len(), 2);
    assert_eq!(history.series[0].event_number, 1);
    assert_eq!(history.series[0].cumulative, 50);
    assert_eq!(history.series[1].cumulative, 40);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_BETS, 40)]));
}

#[test]
fn test_multi_source_merge_preserves_time_order() {
    let history = service(FakeHistoryPort {
        bets: vec![bet(3_000, 100, 1)],
        wheel: vec![WheelHistoryRow {
            spin_time: 1_000,
            result: 20,
            outcome_metadata: None,
        }],
        tips: vec![TipHistoryRow {
            timestamp: 2_000,
            amount: 30,
            fee: 0,
            direction: TipDirection::Received,
            sender_id: 2,
            recipient_id: USER,
        }],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(
        history
            .series
            .iter()
            .map(|point| point.event.source)
            .collect::<Vec<_>>(),
        [SOURCE_WHEEL, SOURCE_TIPS, SOURCE_BETS]
    );
    assert_eq!(
        history
            .series
            .iter()
            .map(|point| point.cumulative)
            .collect::<Vec<_>>(),
        [20, 50, 150]
    );
    assert_eq!(
        history.source_totals,
        BTreeMap::from([(SOURCE_WHEEL, 20), (SOURCE_TIPS, 30), (SOURCE_BETS, 100)])
    );
}

#[test]
fn test_per_source_totals_exclude_zero_net_sources() {
    let history = service(FakeHistoryPort {
        bets: vec![bet(1_000, 50, 1), bet(2_000, -50, 2)],
        wheel: vec![WheelHistoryRow {
            spin_time: 3_000,
            result: 15,
            outcome_metadata: None,
        }],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert!(!history.source_totals.contains_key(SOURCE_BETS));
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_WHEEL, 15)]));
}

#[test]
fn test_prediction_emits_net_event_per_settlement() {
    let history = service(FakeHistoryPort {
        predictions: vec![
            PredictionHistoryRow {
                prediction_id: 1,
                settle_time: Some(1_000),
                delta: 20,
            },
            PredictionHistoryRow {
                prediction_id: 2,
                settle_time: Some(2_000),
                delta: -30,
            },
            PredictionHistoryRow {
                prediction_id: 3,
                settle_time: Some(3_000),
                delta: 0,
            },
        ],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [20, -30]);
    assert_eq!(
        history.source_totals,
        BTreeMap::from([(SOURCE_PREDICTIONS, -10)])
    );
}

#[test]
fn test_wheel_lose_a_turn_results_are_skipped() {
    let history = service(FakeHistoryPort {
        wheel: vec![
            WheelHistoryRow {
                spin_time: 1_000,
                result: 0,
                outcome_metadata: None,
            },
            WheelHistoryRow {
                spin_time: 2_000,
                result: 25,
                outcome_metadata: None,
            },
        ],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [25]);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_WHEEL, 25)]));
}

#[test]
fn test_wheel_history_subtracts_persisted_vanity_tax() {
    let history = service(FakeHistoryPort {
        wheel: vec![WheelHistoryRow {
            spin_time: 1_000,
            result: 100,
            outcome_metadata: Some("{\"vanity_tax\": 1}".to_owned()),
        }],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [99]);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_WHEEL, 99)]));
}

#[test]
fn test_wheel_history_subtracts_both_persisted_profit_taxes() {
    let history = service(FakeHistoryPort {
        wheel: vec![WheelHistoryRow {
            spin_time: 1_000,
            result: 100,
            outcome_metadata: Some("{\"vanity_tax\": 10, \"low_priority_tax\": 10}".to_owned()),
        }],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [80]);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_WHEEL, 80)]));
}

#[test]
fn test_wheel_history_ignores_a_negative_low_priority_tax() {
    let history = service(FakeHistoryPort {
        wheel: vec![WheelHistoryRow {
            spin_time: 1_000,
            result: 100,
            outcome_metadata: Some("{\"low_priority_tax\": -25}".to_owned()),
        }],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [100]);
}

#[test]
fn test_double_or_nothing_delta_from_balance_math() {
    let history = service(FakeHistoryPort {
        doubles: vec![
            DoubleOrNothingHistoryRow {
                spin_time: 1_000,
                cost: 0,
                balance_before: 100,
                balance_after: 200,
                won: true,
            },
            DoubleOrNothingHistoryRow {
                spin_time: 2_000,
                cost: 0,
                balance_before: 50,
                balance_after: 0,
                won: false,
            },
        ],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [100, -50]);
    assert_eq!(
        history.source_totals,
        BTreeMap::from([(SOURCE_DOUBLE_OR_NOTHING, 50)])
    );
}

#[test]
fn test_tip_sent_debits_amount_plus_fee_and_received_credits_amount() {
    let history = service(FakeHistoryPort {
        tips: vec![
            TipHistoryRow {
                timestamp: 1_000,
                amount: 20,
                fee: 2,
                direction: TipDirection::Sent,
                sender_id: USER,
                recipient_id: 2,
            },
            TipHistoryRow {
                timestamp: 2_000,
                amount: 50,
                fee: 5,
                direction: TipDirection::Received,
                sender_id: 3,
                recipient_id: USER,
            },
        ],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [-22, 50]);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_TIPS, 28)]));
}

#[test]
fn test_self_tip_collapses_to_fee_only() {
    let history = service(FakeHistoryPort {
        tips: vec![TipHistoryRow {
            timestamp: 1_000,
            amount: 100,
            fee: 5,
            direction: TipDirection::Sent,
            sender_id: USER,
            recipient_id: USER,
        }],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [-5]);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_TIPS, -5)]));
}

#[test]
fn test_disbursement_events_credit_recipient_amount() {
    let history = service(FakeHistoryPort {
        disbursements: vec![
            DisbursementHistoryRow {
                disbursed_at: 1_000,
                amount: 42,
                method: "even".to_owned(),
            },
            DisbursementHistoryRow {
                disbursed_at: 2_000,
                amount: 0,
                method: "even".to_owned(),
            },
        ],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [42]);
    assert_eq!(
        history.source_totals,
        BTreeMap::from([(SOURCE_DISBURSE, 42)])
    );
}

#[test]
fn test_match_bonuses_collapse_to_one_event_per_match() {
    let history = service(FakeHistoryPort {
        bonuses: vec![
            MatchBonusHistoryRow {
                match_id: 1,
                match_time: Some(1_000),
                won: true,
                bonus_jc: None,
            },
            MatchBonusHistoryRow {
                match_id: 2,
                match_time: Some(2_000),
                won: false,
                bonus_jc: None,
            },
        ],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(history.series.len(), 2);
    assert_eq!(
        history.source_totals.get(SOURCE_BONUS),
        Some(&(LEGACY_WIN_REWARD + LEGACY_PARTICIPATION_REWARD))
    );
    assert!(matches!(
        &history.series[0].event.detail,
        EventDetail::Bonus {
            components: BonusComponents::Reconstructed {
                participation: 0,
                win: LEGACY_WIN_REWARD
            },
            ..
        }
    ));
}

#[test]
fn test_match_bonuses_use_persisted_bonus_jc_when_present() {
    let history = service(FakeHistoryPort {
        bonuses: vec![
            MatchBonusHistoryRow {
                match_id: 1,
                match_time: Some(1_000),
                won: true,
                bonus_jc: Some(1),
            },
            MatchBonusHistoryRow {
                match_id: 2,
                match_time: Some(2_000),
                won: false,
                bonus_jc: Some(0),
            },
        ],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [1]);
    assert_eq!(history.source_totals.get(SOURCE_BONUS), Some(&1));
    assert!(matches!(
        history.series[0].event.detail,
        EventDetail::Bonus {
            components: BonusComponents::PersistedNet(1),
            ..
        }
    ));
}

#[test]
fn test_dig_action_earnings_credit_actor() {
    let history = service(FakeHistoryPort {
        dig: Some(vec![
            dig(USER, None, "dig", Some("{\"jc\":3,\"advance\":2}"), 100, 0),
            dig(USER, None, "dig", Some("{\"jc\":5,\"advance\":4}"), 200, 0),
        ]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [3, 5]);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_DIG, 8)]));
}

#[test]
fn test_dig_cave_in_logs_are_silently_skipped() {
    let history = service(FakeHistoryPort {
        dig: Some(vec![
            dig(
                USER,
                None,
                "dig",
                Some("{\"cave_in\":true,\"block_loss\":3}"),
                100,
                0,
            ),
            dig(USER, None, "dig", Some("{\"jc\":2}"), 200, 0),
        ]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [2]);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_DIG, 2)]));
}

#[test]
fn test_dig_sabotage_trap_debits_actor_and_credits_target() {
    let row = dig(
        USER,
        Some(2),
        "sabotage",
        Some("{\"target_id\":2,\"trap_triggered\":true,\"jc_lost\":10,\"blocks_lost\":3}"),
        100,
        0,
    );
    let actor = service(FakeHistoryPort {
        dig: Some(vec![row.clone()]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    let target = service(FakeHistoryPort {
        dig: Some(vec![row]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(2, GUILD);
    assert_eq!(deltas(&actor), [-10]);
    assert_eq!(deltas(&target), [5]);
}

#[test]
fn test_dig_sabotage_no_trap_debits_cost_and_target_untouched() {
    let row = dig(
        USER,
        Some(2),
        "sabotage",
        Some("{\"target_id\":2,\"damage\":5,\"cost\":7,\"trap_triggered\":false}"),
        100,
        0,
    );
    let actor = service(FakeHistoryPort {
        dig: Some(vec![row.clone()]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    let target = service(FakeHistoryPort {
        dig: Some(vec![row]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(2, GUILD);
    assert_eq!(deltas(&actor), [-7]);
    assert!(target.series.is_empty());
}

#[test]
fn test_dig_boss_fight_wagered_win_returns_net_not_gross() {
    let history = service(FakeHistoryPort {
        dig: Some(vec![dig(
            USER,
            None,
            "boss_fight",
            Some("{\"boundary\":1,\"won\":true,\"wager\":5,\"jc_delta\":15}"),
            100,
            0,
        )]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [10]);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_DIG, 10)]));
}

#[test]
fn test_dig_boss_fight_unwagered_win_uses_full_jc_delta() {
    let history = service(FakeHistoryPort {
        dig: Some(vec![dig(
            USER,
            None,
            "boss_fight",
            Some("{\"boundary\":1,\"won\":true,\"wager\":0,\"jc_delta\":12}"),
            100,
            0,
        )]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [12]);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_DIG, 12)]));
}

#[test]
fn test_dig_boss_fight_phase1_win_with_no_jc_delta_is_zero() {
    let history = service(FakeHistoryPort {
        dig: Some(vec![dig(
            USER,
            None,
            "boss_fight",
            Some("{\"boundary\":1,\"won\":true,\"phase\":1,\"wager\":10}"),
            100,
            0,
        )]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert!(history.series.is_empty());
    assert!(history.source_totals.is_empty());
}

#[test]
fn test_dig_boss_fight_loss_debits_wager() {
    let history = service(FakeHistoryPort {
        dig: Some(vec![dig(
            USER,
            None,
            "boss_fight",
            Some("{\"boundary\":1,\"won\":false,\"wager\":8}"),
            100,
            0,
        )]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [-8]);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_DIG, -8)]));
}

#[test]
fn test_dig_event_handles_jc_delta_cruel_echoes_and_jc_keys() {
    let history = service(FakeHistoryPort {
        dig: Some(vec![
            dig(
                USER,
                None,
                "event",
                Some("{\"event_id\":\"x\",\"choice\":\"A\",\"jc_delta\":4,\"depth_delta\":1}"),
                100,
                0,
            ),
            dig(
                USER,
                None,
                "event",
                Some("{\"event_id\":\"y\",\"cruel_echoes\":true}"),
                200,
                0,
            ),
            dig(
                USER,
                None,
                "event",
                Some("{\"event_id\":\"z\",\"succeeded\":true,\"jc\":6}"),
                300,
                0,
            ),
        ]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [4, -1, 6]);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_DIG, 9)]));
}

#[test]
fn test_dig_help_always_credits_one_jc() {
    let history = service(FakeHistoryPort {
        dig: Some(vec![dig(
            USER,
            Some(2),
            "help",
            Some("{\"target_id\":2,\"advance\":1}"),
            100,
            0,
        )]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [1]);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_DIG, 1)]));
}

#[test]
fn test_dig_abandon_refund_and_retreat_loss() {
    let history = service(FakeHistoryPort {
        dig: Some(vec![
            dig(
                USER,
                None,
                "abandon",
                Some("{\"depth\":5,\"refund\":3}"),
                100,
                0,
            ),
            dig(
                USER,
                None,
                "boss_retreat",
                Some("{\"boundary\":1,\"loss\":2}"),
                200,
                0,
            ),
        ]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [3, -2]);
    assert_eq!(history.source_totals, BTreeMap::from([(SOURCE_DIG, 1)]));
}

#[test]
fn test_dig_prestige_and_unknown_action_types_skipped() {
    let history = service(FakeHistoryPort {
        dig: Some(vec![
            dig(
                USER,
                None,
                "prestige",
                Some("{\"level\":2,\"perk\":\"fast_dig\"}"),
                100,
                0,
            ),
            dig(USER, None, "nonsense_made_up", Some("{\"jc\":100}"), 200, 0),
        ]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert!(history.series.is_empty());
    assert!(history.source_totals.is_empty());
}

#[test]
fn test_dig_jc_delta_column_overrides_json_when_populated() {
    let history = service(FakeHistoryPort {
        dig: Some(vec![dig(USER, None, "dig", Some("{\"jc\":2}"), 100, 999)]),
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(deltas(&history), [999]);
}

#[test]
fn test_dig_missing_repo_produces_no_dig_events() {
    let history = service(FakeHistoryPort {
        dig: None,
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert!(history.series.is_empty());
    assert!(history.source_totals.is_empty());
}

#[test]
fn test_cumulative_series_starts_at_zero_and_sums_correctly() {
    let history = service(FakeHistoryPort {
        bets: vec![bet(1_000, 10, 1), bet(2_000, -5, 2), bet(3_000, 20, 3)],
        ..FakeHistoryPort::default()
    })
    .get_balance_event_series(USER, GUILD);
    assert_eq!(
        history
            .series
            .iter()
            .map(|point| point.cumulative)
            .collect::<Vec<_>>(),
        [10, 5, 25]
    );
    assert_eq!(
        history
            .series
            .iter()
            .map(|point| point.event_number)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(
        history
            .chart_points()
            .iter()
            .map(|point| point.cumulative)
            .collect::<Vec<_>>(),
        [10, 5, 25]
    );
    let png = crate::drawing::draw_balance_chart(
        "HistoryUser",
        &history.chart_points(),
        &history.chart_totals(),
    )
    .into_inner();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
}
