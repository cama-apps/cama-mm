use std::sync::{Arc, Barrier};
use std::thread;

use super::*;

const TEST_GUILD_ID: i64 = 424_242;

type TestService =
    EconomyEventService<MemoryEventStore, MemoryEconomy, FixedClock, ScriptedEventRng>;

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-12,
        "expected {actual} to be within 1e-12 of {expected}"
    );
}

fn local_timestamp(year: i32, month: u32, day: u32, hour: u8, minute: u32) -> i64 {
    let date = NaiveDate::from_ymd_opt(year, month, day).expect("valid fixture date");
    pacific_timestamp(date, hour).expect("valid fixture timestamp") + i64::from(minute) * 60
}

fn seed_economy(economy: &MemoryEconomy, guild_id: GuildId) {
    economy
        .add_player(guild_id, 1, 1_000)
        .expect("seed first player");
    economy
        .add_player(guild_id, 2, 500)
        .expect("seed second player");
    economy.set_reserve(guild_id, 1_000).expect("seed reserve");
}

fn service_at(
    now: i64,
    config: EconomyEventConfig,
) -> (TestService, MemoryEventStore, MemoryEconomy, FixedClock) {
    let store = MemoryEventStore::default();
    let economy = MemoryEconomy::default();
    seed_economy(&economy, GuildId(TEST_GUILD_ID));
    let clock = FixedClock::new(now);
    let service = EconomyEventService::new(
        store.clone(),
        economy.clone(),
        clock.clone(),
        ScriptedEventRng::new([0]),
        config,
    );
    (service, store, economy, clock)
}

fn ravage_draft(service: &TestService, event_date: &str, stock: i64, now: i64) -> EventDraft {
    let (starts_at, ends_at) = service.event_window(event_date).expect("event window");
    EventDraft {
        event_date: event_date.into(),
        name: "Ravage".into(),
        hero: "Tidehunter".into(),
        direction: Direction::Deflationary,
        severity: 2,
        target_effect_jc: -100,
        forecast_flow_jc: 90,
        expected_effect_jc: -110,
        monetary_stock_before: stock,
        effects: EconomyEventEffects {
            reward_multiplier: 0.8,
            gamba_win_multiplier: 0.9,
            gamba_loss_multiplier: 1.1,
            bet_payout_multiplier: 0.97,
            prediction_payout_multiplier: 0.99,
            prediction_depth_multiplier: 1.0,
            prediction_spread_ticks_delta: 2,
            reserve_burn_jc: 100,
            reserve_release_jc: 0,
            wallet_burn_rate: 0.01,
            wallet_burn_jc: 0,
            reserve_voting_disabled: false,
        },
        announcement: "A tidal shock hits the economy.".into(),
        starts_at,
        ends_at,
        created_at: now,
    }
}

fn activate_draft(
    store: &MemoryEventStore,
    economy: &MemoryEconomy,
    guild_id: GuildId,
    draft: EventDraft,
) -> (EconomyEvent, bool) {
    let (event, created) = match store.claim_event(guild_id, draft).expect("claim event") {
        EventClaim::Claimed(event) => (event, true),
        EventClaim::Existing(event) => (event, false),
    };
    if event.status == EventStatus::Active {
        return (event, false);
    }
    let applied = economy
        .apply_event_atomic(guild_id, &event)
        .expect("apply event");
    (
        store
            .activate_event(guild_id, event.event_id, applied)
            .expect("activate event"),
        created,
    )
}

fn sheet_with_stock(stock: i64) -> BalanceSheet {
    BalanceSheet {
        monetary_stock: stock,
        ..BalanceSheet::default()
    }
}

#[test]
fn test_every_economy_event_has_dotabase_spell_art() {
    assert_eq!(EVENT_CATALOG.len(), 15);
    assert!(EVENT_CATALOG.iter().all(|template| {
        let url = template.ability_icon_url();
        !template.ability_icon_slug.is_empty() && url.ends_with(".png")
    }));
}

#[test]
fn test_balance_sheet_counts_reserve_and_average() {
    let economy = MemoryEconomy::default();
    seed_economy(&economy, GuildId(TEST_GUILD_ID));
    let sheet = economy
        .capture_balance_sheet(GuildId(TEST_GUILD_ID))
        .expect("balance sheet");
    assert_eq!(sheet.player_wallets, 1_500);
    assert_eq!(sheet.positive_wallets, 1_500);
    assert_eq!(sheet.player_count, 2);
    assert_close(sheet.average_wallet, 750.0);
    assert_eq!(sheet.reserve_available, 1_000);
    assert_eq!(sheet.monetary_stock, 2_500);
}

#[test]
fn test_reward_volume_includes_trivia_and_generated_mana() {
    let economy = MemoryEconomy::default();
    let now = 2_000_000;
    for (source, amount) in [
        ("dig", 10),
        ("trivia", 20),
        ("player_trivia", 30),
        ("mana_reward", 40),
        ("manashop_buff", 50),
    ] {
        economy
            .add_ledger_entry(LedgerEntry {
                guild_id: GuildId(TEST_GUILD_ID),
                account: LedgerAccount::Player(1),
                delta: amount,
                source: source.into(),
                created_at: now,
            })
            .expect("ledger entry");
    }
    let volumes = economy
        .surface_daily_volumes(GuildId(TEST_GUILD_ID), 1, now)
        .expect("daily volumes");
    assert_close(volumes.reward_credits, 150.0);
}

#[test]
fn test_live_monetary_trends_use_nearest_daily_snapshot_anchors() {
    let store = MemoryEventStore::default();
    let now = local_timestamp(2026, 7, 18, 14, 0);
    for (days_ago, stock) in [(1, 1_050), (3, 1_000), (7, 900)] {
        store
            .seed_snapshot(Snapshot {
                guild_id: GuildId(TEST_GUILD_ID),
                snapshot_date: chrono::DateTime::from_timestamp(
                    now - days_ago * SECONDS_PER_DAY,
                    0,
                )
                .expect("fixture timestamp")
                .date_naive()
                .to_string(),
                captured_at: now - days_ago * SECONDS_PER_DAY,
                balance_sheet: sheet_with_stock(stock),
            })
            .expect("snapshot");
    }
    let trends = store
        .monetary_trends(GuildId(TEST_GUILD_ID), 1_100, now)
        .expect("trends");
    let one = trends[&1].as_ref().expect("one-day trend");
    assert_eq!(one.change_jc, 50);
    assert_close(one.period_rate, 1_100.0 / 1_050.0 - 1.0);
    let three = trends[&3].as_ref().expect("three-day trend");
    assert_close(three.elapsed_days, 3.0);
    assert_close(three.average_daily_change_jc, 100.0 / 3.0);
    let seven = trends[&7].as_ref().expect("seven-day trend");
    assert_eq!(seven.change_jc, 200);
    assert_close(seven.period_rate, 1_100.0 / 900.0 - 1.0);
}

#[test]
fn test_monetary_trends_report_missing_windows_without_fake_rates() {
    let store = MemoryEventStore::default();
    let trends = store
        .monetary_trends(
            GuildId(TEST_GUILD_ID),
            1_000,
            local_timestamp(2026, 7, 18, 14, 0),
        )
        .expect("trends");
    assert_eq!(trends, BTreeMap::from([(1, None), (3, None), (7, None)]));
}

#[test]
fn test_flow_indicators_measure_rolling_turnover_and_participation() {
    let economy = MemoryEconomy::default();
    let now = 5_000_000;
    for (account, delta, source, age) in [
        (LedgerAccount::Player(1), 100, "dig", SECONDS_PER_DAY),
        (LedgerAccount::Player(2), -40, "gamba", 2 * SECONDS_PER_DAY),
        (
            LedgerAccount::Nonprofit,
            60,
            "tax_fine",
            5 * SECONDS_PER_DAY,
        ),
        (
            LedgerAccount::Player(3),
            999,
            "ledger_backfill",
            SECONDS_PER_DAY,
        ),
    ] {
        economy
            .add_ledger_entry(LedgerEntry {
                guild_id: GuildId(TEST_GUILD_ID),
                account,
                delta,
                source: source.into(),
                created_at: now - age,
            })
            .expect("ledger entry");
    }
    let flows = economy
        .flow_indicators(GuildId(TEST_GUILD_ID), 1_000, 4, now)
        .expect("flows");
    let three = &flows[&3];
    assert_eq!(three.net_flow_jc, 60);
    assert_eq!(three.gross_flow_jc, 140);
    assert_eq!(three.active_players, 2);
    assert_close(three.active_player_rate, 0.5);
    assert_close(three.daily_turnover_rate, 140.0 / 3.0 / 1_000.0);
    assert_eq!(flows[&7].net_flow_jc, 120);
    assert_eq!(flows[&7].gross_flow_jc, 200);
}

#[test]
fn test_distribution_indicators_report_concentration() {
    let economy = MemoryEconomy::default();
    for (player_id, balance) in [10, 20, 30, 40, 100].into_iter().enumerate() {
        economy
            .add_player(
                GuildId(TEST_GUILD_ID),
                i64::try_from(player_id + 1).expect("small fixture ID"),
                balance,
            )
            .expect("player");
    }
    let distribution = economy
        .distribution_indicators(GuildId(TEST_GUILD_ID))
        .expect("distribution");
    assert_eq!(distribution.positive_player_count, 5);
    assert_close(distribution.median_positive_wallet, 30.0);
    assert_eq!(distribution.top_decile_count, 1);
    assert_close(distribution.top_decile_share, 0.5);
    assert_close(distribution.gini, 0.4);
}

#[test]
fn test_atomic_event_burns_once_and_records_ledger() {
    let now = local_timestamp(2026, 7, 18, 10, 0);
    let (service, store, economy, _) = service_at(now, EconomyEventConfig::default());
    let draft = ravage_draft(&service, "2026-07-18", 2_500, now);
    let (first, created) = activate_draft(&store, &economy, GuildId(TEST_GUILD_ID), draft.clone());
    let (second, created_again) = activate_draft(&store, &economy, GuildId(TEST_GUILD_ID), draft);
    assert!(created);
    assert!(!created_again);
    assert_eq!(second.event_id, first.event_id);
    assert_eq!(first.effects.reserve_burn_jc, 100);
    assert_eq!(first.effects.wallet_burn_jc, 15);
    assert_eq!(first.direct_effect_jc, -115);
    assert_eq!(
        economy
            .player_balance(GuildId(TEST_GUILD_ID), 1)
            .expect("balance"),
        Some(990)
    );
    assert_eq!(
        economy
            .player_balance(GuildId(TEST_GUILD_ID), 2)
            .expect("balance"),
        Some(495)
    );
    assert_eq!(
        economy.reserve(GuildId(TEST_GUILD_ID)).expect("reserve"),
        900
    );
    assert_eq!(
        economy
            .ledger_totals(GuildId(TEST_GUILD_ID), "economy_event")
            .expect("ledger totals"),
        BTreeMap::from([("nonprofit", -100), ("player", -15)])
    );

    // The same date in another guild has an independent idempotency key and
    // cannot debit the first guild's balances.
    let other = GuildId(TEST_GUILD_ID + 1);
    seed_economy(&economy, other);
    let mut other_draft = ravage_draft(&service, "2026-07-18", 2_500, now);
    other_draft.effects.reserve_burn_jc = 10;
    other_draft.effects.wallet_burn_rate = 0.0;
    activate_draft(&store, &economy, other, other_draft);
    assert_eq!(economy.reserve(other).expect("other reserve"), 990);
    assert_eq!(
        economy.reserve(GuildId(TEST_GUILD_ID)).expect("reserve"),
        900
    );
}

#[test]
fn test_daily_controller_is_idempotent_and_exposes_effects() {
    let now = local_timestamp(2026, 7, 18, 10, 0);
    let (mut service, store, _, _) = service_at(now, EconomyEventConfig::default());
    let (first, created) = service
        .ensure_daily_event(Some(TEST_GUILD_ID), Some(now), None)
        .expect("first event");
    let (second, created_again) = service
        .ensure_daily_event(Some(TEST_GUILD_ID), Some(now), None)
        .expect("second event");
    let first = first.expect("created event");
    let second = second.expect("existing event");
    assert!(created);
    assert!(!created_again);
    assert_eq!(second.event_id, first.event_id);
    assert!(matches!(
        first.direction,
        Direction::Deflationary | Direction::Neutral | Direction::Boon
    ));
    let effects = service.get_effects(Some(TEST_GUILD_ID)).expect("effects");
    assert!(effects.reward_multiplier >= 0.0);
    assert!((0.9..=1.1).contains(&effects.prediction_payout_multiplier));
    assert_eq!(
        store
            .latest_snapshot(GuildId(TEST_GUILD_ID))
            .expect("latest snapshot")
            .expect("snapshot")
            .snapshot_date,
        "2026-07-18"
    );

    // A claimed event survives a transient economy-port failure. A retry sees
    // the failed state, reapplies through the idempotent port, and activates it.
    let (mut recovery, recovery_store, recovery_economy, _) =
        service_at(now, EconomyEventConfig::default());
    recovery_economy
        .fail_next_apply("injected transaction abort")
        .expect("inject failure");
    assert!(matches!(
        recovery.ensure_daily_event(Some(TEST_GUILD_ID), Some(now), None),
        Err(EconomyEventError::Economy(_))
    ));
    let pending = recovery_store
        .event_for_date(GuildId(TEST_GUILD_ID), "2026-07-18")
        .expect("failed event")
        .expect("persisted claim");
    assert_eq!(pending.status, EventStatus::Failed);
    let (recovered, recovered_created) = recovery
        .ensure_daily_event(Some(TEST_GUILD_ID), Some(now), None)
        .expect("recover event");
    assert!(!recovered_created);
    assert_eq!(
        recovered.expect("recovered event").status,
        EventStatus::Active
    );

    // Competing controllers share atomic claim/application ports: exactly one
    // reports creation and a neutral Sunder releases exactly one JC once.
    let concurrent_store = MemoryEventStore::default();
    let concurrent_economy = MemoryEconomy::default();
    seed_economy(&concurrent_economy, GuildId(TEST_GUILD_ID));
    let barrier = Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let store = concurrent_store.clone();
            let economy = concurrent_economy.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let mut controller = EconomyEventService::new(
                    store,
                    economy,
                    FixedClock::new(now),
                    ScriptedEventRng::new([2]),
                    EconomyEventConfig::default(),
                );
                barrier.wait();
                controller
                    .ensure_daily_event(Some(TEST_GUILD_ID), Some(now), None)
                    .expect("concurrent ensure")
                    .1
            })
        })
        .collect();
    let created_count: usize = handles
        .into_iter()
        .map(|handle| usize::from(handle.join().expect("controller thread")))
        .sum();
    assert_eq!(created_count, 1);
    assert_eq!(
        concurrent_economy
            .reserve(GuildId(TEST_GUILD_ID))
            .expect("concurrent reserve"),
        999
    );
}

#[test]
fn test_disabled_service_returns_neutral_effects() {
    let now = local_timestamp(2026, 7, 18, 10, 0);
    let config = EconomyEventConfig {
        enabled: false,
        ..EconomyEventConfig::default()
    };
    let (mut service, _, _, _) = service_at(now, config);
    let (event, created) = service
        .ensure_daily_event(Some(TEST_GUILD_ID), None, None)
        .expect("disabled ensure");
    assert!(event.is_none());
    assert!(!created);
    assert_close(
        service
            .get_effects(Some(TEST_GUILD_ID))
            .expect("neutral effects")
            .reward_multiplier,
        1.0,
    );
}

#[test]
fn test_policy_targets_two_percent_without_a_recovery_override() {
    let now = 1_000;
    let (service, _, _, _) = service_at(now, EconomyEventConfig::default());
    let policy = service
        .ensure_policy(Some(TEST_GUILD_ID))
        .expect("policy state");
    assert_eq!(policy.mode, PolicyMode::Normal);
    assert_close(policy.target_annual_rate, 0.02);
    assert!(policy.recovery_started_at.is_none());
}

#[test]
fn test_active_level_three_deflationary_edict_suspends_reserve_voting() {
    let now = local_timestamp(2026, 7, 18, 12, 0);
    let (service, store, economy, _) = service_at(now, EconomyEventConfig::default());
    let mut draft = ravage_draft(&service, "2026-07-18", 2_500, now);
    draft.severity = 3;
    draft.starts_at = local_timestamp(2026, 7, 18, 10, 0);
    draft.ends_at = local_timestamp(2026, 7, 19, 10, 0);
    let (event, _) = activate_draft(&store, &economy, GuildId(TEST_GUILD_ID), draft);
    assert_eq!(
        service
            .get_reserve_voting_restriction(Some(TEST_GUILD_ID), Some(now))
            .expect("restriction"),
        Some(ReserveVotingRestriction {
            event_id: event.event_id,
            name: "Ravage".into(),
            severity: 3,
        })
    );
}

fn assert_voting_open(direction: Direction, severity: u8, active: bool, enabled: bool) {
    let now = local_timestamp(2026, 7, 18, 12, 0);
    let config = EconomyEventConfig {
        enabled,
        ..EconomyEventConfig::default()
    };
    let (service, store, economy, _) = service_at(now, config);
    let mut draft = ravage_draft(&service, "2026-07-18", 2_500, now);
    draft.direction = direction;
    draft.severity = severity;
    draft.starts_at = local_timestamp(2026, 7, 18, 10, 0);
    draft.ends_at = if active {
        local_timestamp(2026, 7, 19, 10, 0)
    } else {
        now - 1
    };
    activate_draft(&store, &economy, GuildId(TEST_GUILD_ID), draft);
    assert!(
        service
            .get_reserve_voting_restriction(Some(TEST_GUILD_ID), Some(now))
            .expect("restriction lookup")
            .is_none()
    );
}

#[test]
fn test_nonrestrictive_or_inactive_edict_keeps_reserve_voting_open_deflationary_2_true_true() {
    assert_voting_open(Direction::Deflationary, 2, true, true);
}

#[test]
fn test_nonrestrictive_or_inactive_edict_keeps_reserve_voting_open_boon_5_true_true() {
    assert_voting_open(Direction::Boon, 5, true, true);
}

#[test]
fn test_nonrestrictive_or_inactive_edict_keeps_reserve_voting_open_deflationary_5_false_true() {
    assert_voting_open(Direction::Deflationary, 5, false, true);
}

#[test]
fn test_nonrestrictive_or_inactive_edict_keeps_reserve_voting_open_deflationary_5_true_false() {
    assert_voting_open(Direction::Deflationary, 5, true, false);
}

macro_rules! weekly_band_case {
    ($name:ident, $deviation:expr, $direction:expr, $severity:expr) => {
        #[test]
        fn $name() {
            assert_eq!(
                TestService::band_for_weekly_deviation($deviation),
                ($direction, $severity)
            );
        }
    };
}

weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_negative_0_800001_boon_5,
    -0.800_001,
    Direction::Boon,
    5
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_negative_0_8_boon_4,
    -0.80,
    Direction::Boon,
    4
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_negative_0_4_boon_3,
    -0.40,
    Direction::Boon,
    3
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_negative_0_2_boon_2,
    -0.20,
    Direction::Boon,
    2
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_negative_0_1_boon_1,
    -0.10,
    Direction::Boon,
    1
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_negative_0_05_neutral_1,
    -0.05,
    Direction::Neutral,
    1
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_0_0_neutral_1,
    0.0,
    Direction::Neutral,
    1
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_0_05_neutral_1,
    0.05,
    Direction::Neutral,
    1
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_0_050001_deflationary_1,
    0.050_001,
    Direction::Deflationary,
    1
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_0_1_deflationary_1,
    0.10,
    Direction::Deflationary,
    1
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_0_100001_deflationary_2,
    0.100_001,
    Direction::Deflationary,
    2
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_0_2_deflationary_2,
    0.20,
    Direction::Deflationary,
    2
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_0_200001_deflationary_3,
    0.200_001,
    Direction::Deflationary,
    3
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_0_4_deflationary_3,
    0.40,
    Direction::Deflationary,
    3
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_0_400001_deflationary_4,
    0.400_001,
    Direction::Deflationary,
    4
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_0_8_deflationary_4,
    0.80,
    Direction::Deflationary,
    4
);
weekly_band_case!(
    test_weekly_deviation_selects_broad_directional_band_0_800001_deflationary_5,
    0.800_001,
    Direction::Deflationary,
    5
);

fn assert_doom_reward(severity: u8, expected: f64) {
    let now = local_timestamp(2026, 7, 18, 10, 0);
    let (service, _, economy, _) = service_at(now, EconomyEventConfig::default());
    let doom = EVENT_CATALOG
        .iter()
        .find(|template| template.name == "Doom")
        .copied()
        .expect("Doom template");
    let sheet = economy
        .capture_balance_sheet(GuildId(TEST_GUILD_ID))
        .expect("sheet");
    assert_close(
        service
            .effects_for(doom, severity, &sheet)
            .reward_multiplier,
        expected,
    );
}

#[test]
fn test_five_levels_add_granularity_without_exceeding_old_maximum_1_0_925() {
    assert_doom_reward(1, 0.925);
}

#[test]
fn test_five_levels_add_granularity_without_exceeding_old_maximum_2_0_85() {
    assert_doom_reward(2, 0.85);
}

#[test]
fn test_five_levels_add_granularity_without_exceeding_old_maximum_3_0_775() {
    assert_doom_reward(3, 0.775);
}

#[test]
fn test_five_levels_add_granularity_without_exceeding_old_maximum_4_0_6625() {
    assert_doom_reward(4, 0.6625);
}

#[test]
fn test_five_levels_add_granularity_without_exceeding_old_maximum_5_0_55() {
    assert_doom_reward(5, 0.55);
}

fn assert_reserve_voting_effect(severity: u8, expected: bool) {
    let now = local_timestamp(2026, 7, 18, 10, 0);
    let (service, _, economy, _) = service_at(now, EconomyEventConfig::default());
    let doom = EVENT_CATALOG
        .iter()
        .find(|template| template.name == "Doom")
        .copied()
        .expect("Doom template");
    let sheet = economy
        .capture_balance_sheet(GuildId(TEST_GUILD_ID))
        .expect("sheet");
    assert_eq!(
        service
            .effects_for(doom, severity, &sheet)
            .reserve_voting_disabled,
        expected
    );
}

#[test]
fn test_only_severe_deflationary_edicts_disable_reserve_voting_2_false() {
    assert_reserve_voting_effect(2, false);
}

#[test]
fn test_only_severe_deflationary_edicts_disable_reserve_voting_3_true() {
    assert_reserve_voting_effect(3, true);
}

#[test]
fn test_only_severe_deflationary_edicts_disable_reserve_voting_5_true() {
    assert_reserve_voting_effect(5, true);
}

#[test]
fn test_controller_scores_only_the_weekly_band_severity() {
    let now = local_timestamp(2026, 7, 18, 10, 0);
    let (mut service, store, economy, _) = service_at(now, EconomyEventConfig::default());
    store
        .seed_snapshot(Snapshot {
            guild_id: GuildId(TEST_GUILD_ID),
            snapshot_date: "2026-07-11".into(),
            captured_at: now - 7 * SECONDS_PER_DAY,
            balance_sheet: sheet_with_stock(1_000),
        })
        .expect("weekly anchor");
    let (event, created) = service
        .ensure_daily_event(Some(TEST_GUILD_ID), Some(now), Some("2026-07-18"))
        .expect("event");
    let event = event.expect("created event");
    assert!(created);
    assert_eq!(event.direction, Direction::Deflationary);
    assert_eq!(event.severity, 5);
    let template = EVENT_CATALOG
        .iter()
        .find(|template| template.name == event.name)
        .copied()
        .expect("selected template");
    let before = BalanceSheet {
        player_wallets: 1_500,
        positive_wallets: 1_500,
        player_count: 2,
        average_wallet: 750.0,
        reserve_available: 1_000,
        monetary_stock: 2_500,
        ..BalanceSheet::default()
    };
    let level_five = service.effects_for(template, 5, &before);
    assert_eq!(
        event.effects.reward_multiplier,
        level_five.reward_multiplier
    );
    assert_ne!(
        event.effects.reward_multiplier,
        service.effects_for(template, 4, &before).reward_multiplier
    );
    assert_eq!(
        economy
            .capture_balance_sheet(GuildId(TEST_GUILD_ID))
            .expect("post-event sheet")
            .monetary_stock,
        2_500 + event.direct_effect_jc
    );
}

#[test]
fn test_controller_uses_a_neutral_edict_until_seven_day_history_exists() {
    let now = local_timestamp(2026, 7, 18, 10, 0);
    let (mut service, _, _, _) = service_at(now, EconomyEventConfig::default());
    let (event, created) = service
        .ensure_daily_event(Some(TEST_GUILD_ID), Some(now), Some("2026-07-18"))
        .expect("event");
    let event = event.expect("created event");
    assert!(created);
    assert_eq!(event.direction, Direction::Neutral);
    assert_eq!(event.severity, 1);
}

#[test]
fn test_legacy_prediction_depth_effect_is_neutralized() {
    let effects = EconomyEventEffects::from_persisted(&PersistedEconomyEventEffects {
        prediction_depth_multiplier: Some(0.16),
        ..PersistedEconomyEventEffects::default()
    });
    assert_close(effects.prediction_depth_multiplier, 1.0);
}

#[test]
fn test_event_announcement_omits_fixed_prediction_depth() {
    let template = EVENT_CATALOG
        .iter()
        .find(|template| template.name == "Ravage")
        .copied()
        .expect("Ravage template");
    let effects = EconomyEventEffects {
        prediction_payout_multiplier: 0.99,
        prediction_spread_ticks_delta: 2,
        ..EconomyEventEffects::default()
    };
    let announcement = TestService::announcement_text(template, 2, &effects, -100, 100);
    assert!(!announcement.contains("depth"));
    assert!(announcement.contains("resolution **-1.0%**"));
    assert!(announcement.contains("spread **+2 ticks**"));
    assert!(announcement.contains("Observed 7-day stock movement: **+100 JC/day**."));
    assert!(!announcement.to_ascii_lowercase().contains("forecast"));
}

#[test]
fn test_format_event_supports_level_five() {
    let now = local_timestamp(2026, 7, 29, 10, 0);
    let (service, store, economy, _) = service_at(now, EconomyEventConfig::default());
    let mut draft = ravage_draft(&service, "2026-07-29", 1_000, now);
    draft.severity = 5;
    let (event, _) = activate_draft(&store, &economy, GuildId(TEST_GUILD_ID), draft);
    let (title, _) = TestService::format_event(&event);
    assert_eq!(title, "Ravage — Level V");
}

#[test]
fn test_pre_trigger_missing_prior_card_stays_neutral() {
    let now = local_timestamp(2026, 7, 18, 9, 59);
    let (mut service, store, _, _) = service_at(now, EconomyEventConfig::default());
    let (event, created) = service
        .ensure_daily_event(Some(TEST_GUILD_ID), Some(now), None)
        .expect("pre-trigger ensure");
    assert!(event.is_none());
    assert!(!created);
    assert!(
        store
            .event_for_date(GuildId(TEST_GUILD_ID), "2026-07-17")
            .expect("event lookup")
            .is_none()
    );
}

#[test]
fn test_pre_trigger_returns_existing_prior_day_card() {
    let now = local_timestamp(2026, 7, 18, 9, 59);
    let (mut service, store, economy, _) = service_at(now, EconomyEventConfig::default());
    let draft = ravage_draft(&service, "2026-07-17", 2_500, now - SECONDS_PER_DAY);
    let (stored, _) = activate_draft(&store, &economy, GuildId(TEST_GUILD_ID), draft);
    let (event, created) = service
        .ensure_daily_event(Some(TEST_GUILD_ID), Some(now), None)
        .expect("pre-trigger ensure");
    let event = event.expect("prior event");
    assert!(!created);
    assert_eq!(event.event_id, stored.event_id);
    assert_eq!(event.event_date, "2026-07-17");
}

#[test]
fn test_trigger_boundary_creates_new_local_day_card() {
    let now = local_timestamp(2026, 7, 18, 10, 0);
    let (mut service, _, _, _) = service_at(now, EconomyEventConfig::default());
    let (event, created) = service
        .ensure_daily_event(Some(TEST_GUILD_ID), Some(now), None)
        .expect("boundary event");
    let event = event.expect("new event");
    assert!(created);
    assert_eq!(event.event_date, "2026-07-18");
    assert_eq!(event.starts_at, now);
}

#[test]
fn test_get_effects_switches_event_dates_at_ten_am() {
    let before_trigger = local_timestamp(2026, 7, 18, 9, 59);
    let (service, store, economy, clock) =
        service_at(before_trigger, EconomyEventConfig::default());
    let mut prior = ravage_draft(
        &service,
        "2026-07-17",
        2_500,
        before_trigger - SECONDS_PER_DAY,
    );
    prior.effects.reward_multiplier = 0.8;
    activate_draft(&store, &economy, GuildId(TEST_GUILD_ID), prior);
    let mut current = ravage_draft(&service, "2026-07-18", 2_385, before_trigger);
    current.effects.reward_multiplier = 0.6;
    activate_draft(&store, &economy, GuildId(TEST_GUILD_ID), current);
    assert_close(
        service
            .get_effects(Some(TEST_GUILD_ID))
            .expect("prior effects")
            .reward_multiplier,
        0.8,
    );
    clock.set(local_timestamp(2026, 7, 18, 10, 0));
    assert_close(
        service
            .get_effects(Some(TEST_GUILD_ID))
            .expect("current effects")
            .reward_multiplier,
        0.6,
    );
}

#[test]
fn test_explicit_event_date_bypasses_pre_trigger_creation_guard() {
    let now = local_timestamp(2026, 7, 18, 9, 0);
    let (mut service, _, _, _) = service_at(now, EconomyEventConfig::default());
    let (event, created) = service
        .ensure_daily_event(Some(TEST_GUILD_ID), Some(now), Some("2030-01-15"))
        .expect("explicit event");
    let event = event.expect("created event");
    assert!(created);
    assert_eq!(event.event_date, "2030-01-15");
    assert_eq!(
        pacific_local(event.starts_at)
            .expect("local start")
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string(),
        "2030-01-15T10:00:00"
    );
    assert_eq!(
        pacific_local(event.ends_at)
            .expect("local end")
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string(),
        "2030-01-16T10:00:00"
    );
    assert_eq!(
        pacific_offset_for_local(parse_event_date("2030-01-15").unwrap(), 10),
        -28_800
    );
}

fn assert_dst_window(event_date: &str, expected_duration: i64) {
    let now = local_timestamp(2026, 1, 1, 10, 0);
    let (service, _, _, _) = service_at(now, EconomyEventConfig::default());
    let (starts_at, ends_at) = service.event_window(event_date).expect("event window");
    assert_eq!(ends_at - starts_at, expected_duration);
    assert_eq!(pacific_local(starts_at).expect("local start").hour(), 10);
    assert_eq!(pacific_local(ends_at).expect("local end").hour(), 10);
}

#[test]
fn test_event_window_is_dst_aware_2026_03_07_82800() {
    assert_dst_window("2026-03-07", 23 * 60 * 60);
}

#[test]
fn test_event_window_is_dst_aware_2026_10_31_90000() {
    assert_dst_window("2026-10-31", 25 * 60 * 60);
}

fn assert_seconds_to_trigger(year: i32, month: u32, day: u32, expected: i64) {
    let now = local_timestamp(year, month, day, 10, 0);
    let (service, _, _, _) = service_at(now, EconomyEventConfig::default());
    assert_eq!(
        service
            .seconds_until_next_trigger(Some(now))
            .expect("seconds to trigger"),
        expected
    );
}

#[test]
fn test_seconds_until_next_trigger_tracks_dst_2026_3_7_82800() {
    assert_seconds_to_trigger(2026, 3, 7, 23 * 60 * 60);
}

#[test]
fn test_seconds_until_next_trigger_tracks_dst_2026_10_31_90000() {
    assert_seconds_to_trigger(2026, 10, 31, 25 * 60 * 60);
}

#[test]
fn test_estimate_effect_treats_reserve_redistribution_as_supply_neutral() {
    let effects = EconomyEventEffects {
        reserve_release_jc: 250,
        ..EconomyEventEffects::default()
    };
    assert_eq!(
        TestService::estimate_effect(
            &effects,
            &SurfaceVolumes::default(),
            &BalanceSheet::default()
        ),
        0
    );
}

#[test]
fn test_atomic_event_release_credits_players_without_changing_supply() {
    let now = local_timestamp(2026, 7, 18, 10, 0);
    let (service, store, economy, _) = service_at(now, EconomyEventConfig::default());
    let before = economy
        .capture_balance_sheet(GuildId(TEST_GUILD_ID))
        .expect("before sheet");
    let mut draft = ravage_draft(&service, "2026-07-18", before.monetary_stock, now);
    draft.effects.reserve_burn_jc = 0;
    draft.effects.wallet_burn_rate = 0.0;
    draft.effects.reserve_release_jc = 200;
    let (event, created) = activate_draft(&store, &economy, GuildId(TEST_GUILD_ID), draft);
    assert!(created);
    assert_eq!(event.effects.reserve_release_jc, 200);
    assert_eq!(event.direct_effect_jc, 0);
    assert_eq!(
        economy
            .player_balance(GuildId(TEST_GUILD_ID), 1)
            .expect("first balance"),
        Some(1_100)
    );
    assert_eq!(
        economy
            .player_balance(GuildId(TEST_GUILD_ID), 2)
            .expect("second balance"),
        Some(600)
    );
    assert_eq!(
        economy.reserve(GuildId(TEST_GUILD_ID)).expect("reserve"),
        800
    );
    assert_eq!(
        economy
            .capture_balance_sheet(GuildId(TEST_GUILD_ID))
            .expect("after sheet")
            .monetary_stock,
        before.monetary_stock
    );
}

#[test]
fn test_mark_event_announced_stamps_once() {
    let now = local_timestamp(2026, 7, 18, 10, 0);
    let (service, store, economy, _) = service_at(now, EconomyEventConfig::default());
    let draft = ravage_draft(&service, "2026-07-18", 2_500, now);
    let (event, _) = activate_draft(&store, &economy, GuildId(TEST_GUILD_ID), draft);
    assert!(event.announced_at.is_none());
    service
        .mark_event_announced(Some(TEST_GUILD_ID), event.event_id, Some(1_111))
        .expect("first stamp");
    assert_eq!(
        store
            .event_for_date(GuildId(TEST_GUILD_ID), "2026-07-18")
            .expect("event")
            .expect("stored event")
            .announced_at,
        Some(1_111)
    );
    service
        .mark_event_announced(Some(TEST_GUILD_ID), event.event_id, Some(2_222))
        .expect("retry stamp");
    assert_eq!(
        store
            .event_for_date(GuildId(TEST_GUILD_ID), "2026-07-18")
            .expect("event")
            .expect("stored event")
            .announced_at,
        Some(1_111)
    );
}

#[test]
fn test_event_has_one_initial_and_one_twelve_hour_reminder_slot() {
    let now = local_timestamp(2026, 7, 18, 10, 0);
    let (service, store, economy, _) = service_at(now, EconomyEventConfig::default());
    let mut draft = ravage_draft(&service, "2026-07-18", 2_500, now);
    draft.starts_at = 1_000;
    draft.ends_at = 87_400;
    let (event, _) = activate_draft(&store, &economy, GuildId(TEST_GUILD_ID), draft);
    assert_eq!(
        TestService::pending_announcement_slot(&event, 1_000),
        Some("initial")
    );
    service
        .mark_event_announced(Some(TEST_GUILD_ID), event.event_id, Some(1_001))
        .expect("initial stamp");
    let event = store
        .event_for_date(GuildId(TEST_GUILD_ID), "2026-07-18")
        .expect("event")
        .expect("stored event");
    assert_eq!(TestService::pending_announcement_slot(&event, 44_199), None);
    assert_eq!(
        TestService::pending_announcement_slot(&event, 44_200),
        Some("reminder")
    );
    service
        .mark_event_reminder_announced(Some(TEST_GUILD_ID), event.event_id, Some(44_201))
        .expect("reminder stamp");
    let event = store
        .event_for_date(GuildId(TEST_GUILD_ID), "2026-07-18")
        .expect("event")
        .expect("stored event");
    assert_eq!(event.reminder_announced_at, Some(44_201));
    assert_eq!(TestService::pending_announcement_slot(&event, 44_202), None);
}

#[test]
fn deterministic_event_rng_matches_python_sha256_mersenne_choice() {
    // Pinned against CPython:
    //   seed = int.from_bytes(sha256(f"{guild}:{date}".encode()).digest()[:8], "big")
    //   random.Random(seed).choice(range(n))
    let mut rng = DeterministicEventRng;
    for (guild, date, upper, expected) in [
        (
            1_234_567_890_123_456_789_i64,
            "2026-08-01",
            3_usize,
            2_usize,
        ),
        (1_234_567_890_123_456_789, "2026-08-03", 3, 2),
        (0, "2026-01-01", 3, 1),
        (987_654_321, "2026-12-31", 2, 1),
        (42, "2026-06-15", 1, 0),
    ] {
        assert_eq!(
            rng.choose_index(GuildId(guild), date, upper),
            expected,
            "guild {guild} date {date} upper {upper}"
        );
    }
}
