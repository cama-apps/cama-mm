use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use cama_domain::embed_safety::{DESCRIPTION_LIMIT, FIELD_VALUE_LIMIT, validate_embed};
use cama_domain::permissions::{DiscordPermissions, PermissionContext};

use super::*;

fn user(id: u64, display_name: &str) -> UserRef {
    UserRef {
        id,
        name: display_name.to_lowercase(),
        display_name: display_name.to_owned(),
    }
}

fn global_silence_status() -> PolicyStatus {
    PolicyStatus {
        policy: EconomyPolicy {
            mode: "recovery".to_owned(),
            target_annual_rate: -0.035,
            inflation_ceiling: 0.02,
        },
        balance_sheet: BalanceSheet {
            monetary_stock: 124_464,
            player_wallets: 53_900,
            average_wallet: 449.17,
            reserve_available: 42_795,
            reserve_locked: 0,
            prediction_open_cash: 26_155,
            wager_escrow: 1_614,
            ..BalanceSheet::default()
        },
        annualized_30d: None,
        annualized_90d: None,
        monetary_trends: BTreeMap::new(),
        flow_indicators: BTreeMap::new(),
        distribution: Distribution::default(),
        event: Some(EconomyEvent {
            name: "Global Silence".to_owned(),
            severity: 3,
            direction: EconomyDirection::Deflationary,
            announcement: "Bonus rewards vanish and market makers fall quiet.\nGenerated rewards (dig, trivia, and mana): **-36%**."
                .to_owned(),
            effects: EconomyEffects {
                reward_multiplier: 0.64,
                gamba_win_multiplier: 0.91,
                gamba_loss_multiplier: 1.0,
                bet_payout_multiplier: 0.97,
                prediction_payout_multiplier: 1.0,
                prediction_depth_multiplier: 0.16,
                prediction_spread_ticks_delta: 6,
                reserve_voting_disabled: Some(true),
                reserve_burn_jc: 0,
                wallet_burn_jc: 0,
                reserve_release_jc: 0,
            },
            forecast_flow_jc: 2_237,
            target_effect_jc: -2_249,
            expected_effect_jc: -2_203,
            direct_effect_jc: 0,
            ends_at: Some(1_752_943_600),
        }),
    }
}

fn ledger_row(index: i64, reason: Option<String>, source: &str) -> LedgerRow {
    LedgerRow {
        ledger_id: index,
        guild_id: 123,
        account: LedgerAccount::Player(u64::try_from(10_000 + index).unwrap()),
        delta: index + 1,
        balance_before: index * 10,
        balance_after: index * 10 + index + 1,
        source: source.to_owned(),
        actor_id: Some(42),
        related_type: (source == "gamba").then(|| "wheel_spin".to_owned()),
        related_id: (source == "gamba").then(|| "LIGHTNING_BOLT".to_owned()),
        reason,
        created_at: 1_700_000_000 + index,
    }
}

fn player_snapshot(rows: Vec<LedgerRow>) -> PlayerSnapshot {
    PlayerSnapshot {
        balance: 100,
        recent_ledger: rows,
        ..PlayerSnapshot::default()
    }
}

fn tax_man() -> PermissionContext {
    PermissionContext::new(42)
}

fn tax_authorized() -> TaxCommandAuthorization<'static> {
    TaxCommandAuthorization {
        permissions: tax_man(),
        admin_user_ids: &[],
        tax_man_user_ids: &[42],
    }
}

#[derive(Clone)]
struct FakeEconomy {
    status: PolicyStatus,
    calls: Cell<usize>,
    fail: bool,
}

impl EconomyStatusPort for FakeEconomy {
    type Error = ();

    fn get_policy_status(&self, guild_id: u64) -> Result<PolicyStatus, Self::Error> {
        assert_eq!(guild_id, 123);
        self.calls.set(self.calls.get() + 1);
        if self.fail {
            Err(())
        } else {
            Ok(self.status.clone())
        }
    }
}

struct FakeIcons {
    fail: bool,
}

impl SpellIconPort for FakeIcons {
    type Error = ();

    fn get_ability_icon_url(&self, event_name: &str) -> Result<Option<String>, Self::Error> {
        assert_eq!(event_name, "Global Silence");
        if self.fail {
            Err(())
        } else {
            Ok(Some("https://cdn.example/global_silence.png".to_owned()))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ReadCall {
    Count(u64, Option<u64>),
    Recent(u64, usize, usize, Option<u64>),
    Snapshot(u64, u64, usize, usize),
}

struct FakeReadPort {
    calls: RefCell<Vec<ReadCall>>,
    total: usize,
    rows: Vec<LedgerRow>,
    snapshot: PlayerSnapshot,
    fail_count: bool,
    fail_rows: bool,
    fail_snapshot: bool,
}

impl FakeReadPort {
    fn new(total: usize, rows: Vec<LedgerRow>, snapshot: PlayerSnapshot) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            total,
            rows,
            snapshot,
            fail_count: false,
            fail_rows: false,
            fail_snapshot: false,
        }
    }
}

impl LedgerReadPort for FakeReadPort {
    type Error = ();

    fn count_ledger_entries(
        &self,
        guild_id: u64,
        user_id: Option<u64>,
    ) -> Result<usize, Self::Error> {
        self.calls
            .borrow_mut()
            .push(ReadCall::Count(guild_id, user_id));
        if self.fail_count {
            Err(())
        } else {
            Ok(self.total)
        }
    }

    fn get_recent_ledger(
        &self,
        guild_id: u64,
        limit: usize,
        offset: usize,
        user_id: Option<u64>,
    ) -> Result<Vec<LedgerRow>, Self::Error> {
        self.calls
            .borrow_mut()
            .push(ReadCall::Recent(guild_id, limit, offset, user_id));
        if self.fail_rows {
            Err(())
        } else {
            Ok(self.rows.clone())
        }
    }
}

impl PlayerAuditReadPort for FakeReadPort {
    fn get_player_snapshot(
        &self,
        user_id: u64,
        guild_id: u64,
        ledger_limit: usize,
        ledger_offset: usize,
    ) -> Result<PlayerSnapshot, Self::Error> {
        self.calls.borrow_mut().push(ReadCall::Snapshot(
            user_id,
            guild_id,
            ledger_limit,
            ledger_offset,
        ));
        if self.fail_snapshot {
            Err(())
        } else {
            Ok(self.snapshot.clone())
        }
    }
}

#[test]
fn test_tax_group_contains_audit_and_enforcement_commands() {
    assert_eq!(
        TAX_SUBCOMMANDS.into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "audit",
            "event",
            "player",
            "ledger",
            "fine",
            "resetcooldown",
            "bankruptcy",
            "policy",
            "vanity",
        ])
    );
}

#[test]
fn test_public_event_embed_is_high_level_theatrical_and_explains_indirect_effect() {
    let embed = build_public_event_embed(
        &global_silence_status(),
        Some("https://cdn.example/global_silence.png"),
    );
    let text = embed.text();
    assert_eq!(
        embed.model.title.as_deref(),
        Some("🌑 Global Silence — Level III")
    );
    assert_eq!(
        embed.thumbnail_url.as_deref(),
        Some("https://cdn.example/global_silence.png")
    );
    for phrase in [
        "36% lower",
        "9% lower",
        "3% lower",
        "6 ticks wider",
        "Reserve allocation voting is suspended",
        "No JC moved when this spell activated",
    ] {
        assert!(text.contains(phrase), "missing {phrase:?}");
    }
    for private in [
        "thinner",
        "Forecast unmanaged flow",
        "Target event effect",
        "124,464",
    ] {
        assert!(!text.contains(private), "leaked {private:?}");
    }
    assert!(validate_embed(&embed.model).is_empty());
}

#[test]
fn test_public_event_embed_supports_level_five_and_explains_policy_window() {
    let mut status = global_silence_status();
    status.event.as_mut().unwrap().severity = 5;
    let embed = build_public_event_embed(&status, None);
    assert_eq!(
        embed.model.title.as_deref(),
        Some("🌑 Global Silence — Level V")
    );
    assert!(embed.text().contains("2% annual target"));
    assert!(embed.text().contains("rolling 7-day change"));
}

#[test]
fn test_legacy_severe_deflationary_edict_still_displays_voting_suspension() {
    let mut status = global_silence_status();
    status
        .event
        .as_mut()
        .unwrap()
        .effects
        .reserve_voting_disabled = None;
    assert!(
        build_public_event_embed(&status, None)
            .text()
            .contains("Reserve allocation voting is suspended")
    );
}

#[test]
fn test_public_event_embed_names_immediate_reserve_and_wallet_actions() {
    let mut status = global_silence_status();
    let effects = &mut status.event.as_mut().unwrap().effects;
    *effects = EconomyEffects {
        reserve_burn_jc: 300,
        wallet_burn_jc: 75,
        ..EconomyEffects::neutral()
    };
    let text = build_public_event_embed(&status, None).text();
    assert!(text.contains("300 JC"));
    assert!(text.contains("burned from the Jopa Reserve"));
    assert!(text.contains("75 JC"));
    assert!(text.contains("burned from positive wallets"));
    assert!(!text.contains("No JC moved"));
}

#[test]
fn test_public_event_embed_handles_no_active_event() {
    let mut status = global_silence_status();
    status.event = None;
    let embed = build_public_event_embed(&status, None);
    assert_eq!(
        embed.model.title.as_deref(),
        Some("🌤️ The Economy Is Between Spells")
    );
    assert!(
        embed
            .model
            .description
            .as_deref()
            .unwrap()
            .contains("10 AM Pacific")
    );
    assert!(validate_embed(&embed.model).is_empty());
}

#[test]
fn test_private_policy_embed_labels_zero_direct_effect_as_indirect() {
    let text = build_policy_embed(&global_silence_status()).text();
    for phrase in [
        "Projected daily impact",
        "Immediate supply change",
        "None",
        "works through adjusted outcomes",
    ] {
        assert!(text.contains(phrase));
    }
}

#[test]
fn test_private_policy_embed_shows_granular_macro_signals() {
    let mut status = global_silence_status();
    status.balance_sheet.player_count = 120;
    status.balance_sheet.positive_wallets = 60_000;
    status.balance_sheet.visible_debt = 6_000;
    status.balance_sheet.reserve_next_match_pot = 500;
    status.monetary_trends = BTreeMap::from([
        (
            "1d",
            MonetaryTrend {
                elapsed_days: 1.0,
                period_rate: 0.01,
                daily_rate: 0.01,
                change_jc: 1_232,
                average_daily_change_jc: 1_232.0,
            },
        ),
        (
            "3d",
            MonetaryTrend {
                elapsed_days: 3.0,
                period_rate: 0.024,
                daily_rate: 0.00794,
                change_jc: 2_916,
                average_daily_change_jc: 972.0,
            },
        ),
        (
            "7d",
            MonetaryTrend {
                elapsed_days: 7.0,
                period_rate: 0.035,
                daily_rate: 0.00493,
                change_jc: 4_209,
                average_daily_change_jc: 601.3,
            },
        ),
    ]);
    status.flow_indicators = BTreeMap::from([
        (
            "3d",
            FlowIndicator {
                average_daily_net_jc: 825.0,
                average_daily_gross_jc: 4_200.0,
                daily_turnover_rate: 0.03375,
                active_players: 0,
                active_player_rate: 0.0,
            },
        ),
        (
            "7d",
            FlowIndicator {
                average_daily_net_jc: 600.0,
                average_daily_gross_jc: 3_750.0,
                daily_turnover_rate: 0.03013,
                active_players: 48,
                active_player_rate: 0.4,
            },
        ),
    ]);
    status.distribution = Distribution {
        median_positive_wallet: 250.0,
        top_decile_share: 0.58,
        gini: 0.612,
    };
    let embed = build_policy_embed(&status);
    let text = embed.text();
    for phrase in [
        "Money-supply inflation proxy",
        "Live ~24h",
        "Rolling 3d",
        "Rolling 7d",
        "Momentum: **heating**",
        "ledger movement",
        "48/120 players",
        "Visible debt / positive wallets: **10.0%**",
        "Positive-wallet top 10% share: **58.0%**",
        "positive-wallet Gini: **0.612**",
        "not a price index",
    ] {
        assert!(text.contains(phrase), "missing {phrase:?}");
    }
    assert!(validate_embed(&embed.model).is_empty());
}

#[test]
fn test_tax_event_is_public_and_does_not_require_tax_man() {
    let economy = FakeEconomy {
        status: global_silence_status(),
        calls: Cell::new(0),
        fail: false,
    };
    let response = event_command(123, Some(&economy), &FakeIcons { fail: false });
    assert!(!response.ephemeral);
    assert!(response.deferred);
    assert_eq!(economy.calls.get(), 1);
    assert_eq!(
        response.embed.unwrap().model.title.as_deref(),
        Some("🌑 Global Silence — Level III")
    );
}

#[test]
fn test_tax_event_survives_spell_icon_lookup_failure() {
    let economy = FakeEconomy {
        status: global_silence_status(),
        calls: Cell::new(0),
        fail: false,
    };
    let embed = event_command(123, Some(&economy), &FakeIcons { fail: true })
        .embed
        .unwrap();
    assert_eq!(
        embed.model.title.as_deref(),
        Some("🌑 Global Silence — Level III")
    );
    assert!(embed.thumbnail_url.is_none());
}

#[test]
fn test_tax_policy_remains_private_and_tax_man_only() {
    let economy = FakeEconomy {
        status: global_silence_status(),
        calls: Cell::new(0),
        fail: false,
    };
    let response = policy_command(
        123,
        &TaxCommandAuthorization {
            permissions: PermissionContext::new(42),
            admin_user_ids: &[],
            tax_man_user_ids: &[],
        },
        Some(&economy),
    );
    assert_eq!(economy.calls.get(), 0);
    assert!(response.ephemeral);
    assert_eq!(
        response.content.as_deref(),
        Some("Only Tax Men can use this command.")
    );
}

#[test]
fn test_tax_player_recent_ledger_splits_long_field() {
    let rows = (0..12)
        .map(|index| {
            ledger_row(
                index,
                Some(format!("gamba lightning bolt tax {}", "x".repeat(120))),
                "gamba",
            )
        })
        .collect();
    let embed = build_player_embed(
        &user(99, "Taxpayer"),
        &player_snapshot(rows),
        1,
        None,
        PLAYER_LEDGER_DEFAULT_LIMIT,
    );
    let recent = embed
        .model
        .fields
        .iter()
        .filter(|field| field.name == "Recent Ledger" || field.name == "\u{200b}")
        .collect::<Vec<_>>();
    assert!(recent.len() > 1);
    assert!(
        recent
            .iter()
            .all(|field| field.value.chars().count() <= FIELD_VALUE_LIMIT)
    );
    assert!(validate_embed(&embed.model).is_empty());
}

#[test]
fn test_ledger_rows_prefer_descriptive_reason() {
    let text = format_ledger_rows(&[ledger_row(
        1,
        Some("gamba lightning bolt tax".to_owned()),
        "gamba",
    )]);
    assert!(text.contains("gamba lightning bolt tax"));
    assert!(!text.contains("via `balance_update`"));
}

#[test]
fn test_ledger_embed_truncates_description_to_discord_limit() {
    let rows = (0..25)
        .map(|index| {
            ledger_row(
                index,
                Some(format!("dig event credit {}", "y".repeat(240))),
                "dig",
            )
        })
        .collect::<Vec<_>>();
    let embed = build_ledger_embed(&rows, None, 1, None, LEDGER_DEFAULT_LIMIT);
    assert!(embed.model.description.as_deref().unwrap().chars().count() <= DESCRIPTION_LIMIT);
    assert!(validate_embed(&embed.model).is_empty());
}

#[test]
fn test_ledger_pagination_helpers_clamp_page_and_calculate_offset() {
    assert_eq!(ledger_total_pages(0, 10), 1);
    assert_eq!(ledger_total_pages(26, 25), 2);
    assert_eq!(clamp_ledger_page(99, 10, 21), 3);
    assert_eq!(ledger_offset(3, 10), 20);
}

#[test]
fn test_ledger_embed_includes_page_footer() {
    let rows = (0..10)
        .map(|index| ledger_row(index, Some(format!("entry {index}")), "balance_update"))
        .collect::<Vec<_>>();
    let embed = build_ledger_embed(&rows, None, 2, Some(27), 10);
    assert_eq!(
        embed.model.footer.as_deref(),
        Some("Tax Man ledger | Page 2/3 | Entries 11-20 of 27")
    );
    assert!(validate_embed(&embed.model).is_empty());
}

#[test]
fn test_ledger_empty_embed_reports_zero_entry_page() {
    let embed = build_ledger_embed(&[], None, 9, Some(0), 10);
    assert_eq!(
        embed.model.description.as_deref(),
        Some("No ledger entries yet.")
    );
    assert_eq!(
        embed.model.footer.as_deref(),
        Some("Tax Man ledger | Page 1/1 | 0 entries")
    );
}

#[test]
fn test_ledger_embed_includes_page_and_limit_metadata() {
    let rows = [
        ledger_row(30, None, "balance_update"),
        ledger_row(31, None, "balance_update"),
    ];
    let embed = build_ledger_embed(&rows, None, 4, Some(100), 13);
    let text = embed.text();
    assert!(text.contains("Page 4"));
    assert!(text.contains("Entries 40-52 of 100"));
    assert!(validate_embed(&embed.model).is_empty());
}

#[test]
fn test_ledger_embed_empty_high_page_clamps_to_available_page() {
    let embed = build_ledger_embed(&[], None, 99, Some(0), 10);
    let text = embed.text();
    assert!(text.contains("No ledger entries yet."));
    assert!(text.contains("Page 1/1"));
    assert!(text.contains("0 entries"));
    assert!(validate_embed(&embed.model).is_empty());
}

#[test]
fn test_ledger_offset_uses_one_indexed_pages() {
    assert_eq!(ledger_offset(1, 10), 0);
    assert_eq!(ledger_offset(3, 7), 14);
    assert_eq!(ledger_offset(0, 10), 0);
}

#[test]
fn test_tax_ledger_page_calculates_offset_for_service() {
    let port = FakeReadPort::new(
        100,
        vec![ledger_row(14, None, "balance_update")],
        player_snapshot(Vec::new()),
    );
    let output = ledger_command(
        &port,
        &tax_authorized(),
        LedgerCommandRequest {
            guild_id: 123,
            requester_id: 42,
            user: None,
            page: 3,
            limit: 7,
        },
    );
    assert_eq!(
        port.calls.borrow().as_slice(),
        [
            ReadCall::Count(123, None),
            ReadCall::Recent(123, 7, 14, None),
        ]
    );
    assert!(output.response.ephemeral);
}

#[test]
fn test_tax_player_uses_paginated_recent_ledger_slice() {
    let limit = PLAYER_LEDGER_DEFAULT_LIMIT;
    let snapshot = player_snapshot(
        (0..i64::try_from(limit).unwrap())
            .map(|index| ledger_row(index, None, "balance_update"))
            .collect(),
    );
    let port = FakeReadPort::new(17, Vec::new(), snapshot);
    let output = player_command(
        &port,
        &tax_authorized(),
        PlayerCommandRequest {
            guild_id: 123,
            requester_id: 42,
            user: user(99, "Taxpayer"),
        },
    );
    assert_eq!(
        port.calls.borrow().as_slice(),
        [
            ReadCall::Snapshot(99, 123, limit, 0),
            ReadCall::Count(123, Some(99)),
        ]
    );
    let view = output.view.unwrap();
    assert!(view.buttons.previous_disabled);
    assert!(!view.buttons.next_disabled);
    let embed = output.response.embed.unwrap();
    assert!(embed.text().contains("Page 1"));
    assert!(embed.text().contains("Entries 1-8 of 17"));
    assert!(validate_embed(&embed.model).is_empty());
}

#[test]
fn test_tax_player_ledger_view_loads_next_page_with_offset() {
    let limit = PLAYER_LEDGER_DEFAULT_LIMIT;
    let port = FakeReadPort::new(
        21,
        Vec::new(),
        player_snapshot(
            (16..21)
                .map(|index| ledger_row(index, None, "balance_update"))
                .collect(),
        ),
    );
    let mut view = TaxPlayerLedgerView::new(123, 42, user(99, "Taxpayer"), limit, 1, 21);
    let result = view.show_page(&port, 3);
    assert_eq!(
        port.calls.borrow().as_slice(),
        [
            ReadCall::Count(123, Some(99)),
            ReadCall::Snapshot(99, 123, limit, 16),
        ]
    );
    assert!(result.deferred);
    assert!(view.buttons.next_disabled);
    assert!(!view.buttons.previous_disabled);
    let embed = result.edited_embed.unwrap();
    assert!(embed.text().contains("Page 3"));
    assert!(embed.text().contains("Entries 17-21 of 21"));
    assert!(validate_embed(&embed.model).is_empty());
}

#[test]
fn test_tax_player_ledger_view_failed_load_keeps_page_state() {
    let mut port = FakeReadPort::new(21, Vec::new(), player_snapshot(Vec::new()));
    port.fail_snapshot = true;
    let mut view = TaxPlayerLedgerView::new(
        123,
        42,
        user(99, "Taxpayer"),
        PLAYER_LEDGER_DEFAULT_LIMIT,
        1,
        21,
    );
    let original = view.clone();
    let result = view.show_page(&port, 3);
    assert!(result.deferred);
    assert_eq!(view, original);
    assert!(result.edited_embed.is_none());
    assert!(result.ephemeral_error.unwrap().contains("Couldn't load"));
}

#[test]
fn test_tax_ledger_view_failed_load_keeps_page_state() {
    let mut port = FakeReadPort::new(100, Vec::new(), player_snapshot(Vec::new()));
    port.fail_count = true;
    let mut view = TaxLedgerView::new(123, 42, None, 10, 2, 100);
    let original = view.clone();
    let result = view.show_page(&port, 3);
    assert!(result.deferred);
    assert_eq!(view, original);
    assert_eq!(port.calls.borrow().as_slice(), [ReadCall::Count(123, None)]);
    assert!(result.edited_embed.is_none());
    assert!(result.ephemeral_error.unwrap().contains("Couldn't load"));
}

struct FakeEnforcement {
    fine_requests: RefCell<Vec<FineRequest>>,
    reset_requests: RefCell<Vec<ResetFineCooldownRequest>>,
    bankruptcy_requests: RefCell<Vec<BankruptcyRequest>>,
    fine_result: FineResult,
    reset_result: ResetFineCooldownResult,
    bankruptcy_result: BankruptcyResult,
}

impl FakeEnforcement {
    fn new() -> Self {
        Self {
            fine_requests: RefCell::new(Vec::new()),
            reset_requests: RefCell::new(Vec::new()),
            bankruptcy_requests: RefCell::new(Vec::new()),
            fine_result: FineResult::Rejected,
            reset_result: ResetFineCooldownResult::Rejected,
            bankruptcy_result: BankruptcyResult::Rejected,
        }
    }
}

impl TaxEnforcementPort for FakeEnforcement {
    type Error = Infallible;

    fn levy_fine_atomic(&self, request: FineRequest) -> Result<FineResult, Self::Error> {
        self.fine_requests.borrow_mut().push(request);
        Ok(self.fine_result.clone())
    }

    fn reset_fine_cooldown_atomic(
        &self,
        request: ResetFineCooldownRequest,
    ) -> Result<ResetFineCooldownResult, Self::Error> {
        self.reset_requests.borrow_mut().push(request);
        Ok(self.reset_result.clone())
    }

    fn change_bankruptcy_modifier_atomic(
        &self,
        request: BankruptcyRequest,
    ) -> Result<BankruptcyResult, Self::Error> {
        self.bankruptcy_requests.borrow_mut().push(request);
        Ok(self.bankruptcy_result.clone())
    }
}

#[test]
fn test_tax_fine_calls_service_and_reports_exact_deduction() {
    let mut port = FakeEnforcement::new();
    port.fine_result = FineResult::Ok {
        requested_amount: 10,
        applied_amount: 10,
        balance_before: 7,
        balance_after: -3,
        next_fine_at: Some(1_702_592_000),
    };
    let response = fine_command(
        &port,
        &tax_authorized(),
        FineCommandRequest {
            target: user(99, "Taxpayer"),
            guild_id: 123,
            actor_id: 42,
            amount: 10,
            reason: Some("failure to file".to_owned()),
        },
    );
    assert_eq!(
        port.fine_requests.borrow().as_slice(),
        [FineRequest {
            user_id: 99,
            guild_id: 123,
            amount: 10,
            actor_id: 42,
            reason: Some("failure to file".to_owned()),
        }]
    );
    let message = response.content.unwrap();
    for phrase in [
        "Levied a 10",
        "Jopacoin Reserve",
        "Balance: 7",
        "-3",
        "<t:1702592000:R>",
    ] {
        assert!(message.contains(phrase), "missing {phrase:?}");
    }
    assert!(response.ephemeral);
}

#[test]
fn test_tax_fine_reports_cooldown() {
    let mut port = FakeEnforcement::new();
    port.fine_result = FineResult::Cooldown {
        next_fine_at: 1_702_592_000,
    };
    let response = fine_command(
        &port,
        &tax_authorized(),
        FineCommandRequest {
            target: user(99, "Taxpayer"),
            guild_id: 123,
            actor_id: 42,
            amount: 10,
            reason: None,
        },
    );
    let message = response.content.unwrap();
    assert!(message.contains("still on Tax Man fine cooldown"));
    assert!(message.contains("<t:1702592000:f>"));
}

#[test]
fn test_tax_resetcooldown_calls_service() {
    let mut port = FakeEnforcement::new();
    port.reset_result = ResetFineCooldownResult::Ok { had_cooldown: true };
    let response = resetcooldown_command(
        &port,
        &tax_authorized(),
        ResetCooldownCommandRequest {
            target: user(99, "Taxpayer"),
            guild_id: 123,
            actor_id: 42,
        },
    );
    assert_eq!(
        port.reset_requests.borrow().as_slice(),
        [ResetFineCooldownRequest {
            user_id: 99,
            guild_id: 123,
            actor_id: 42,
        }]
    );
    assert!(
        response
            .content
            .as_deref()
            .unwrap()
            .contains("Reset Tax Man fine cooldown for Taxpayer")
    );
    assert!(response.ephemeral);
}

#[test]
fn test_tax_resetcooldown_reports_already_clear() {
    let mut port = FakeEnforcement::new();
    port.reset_result = ResetFineCooldownResult::Ok {
        had_cooldown: false,
    };
    let response = resetcooldown_command(
        &port,
        &tax_authorized(),
        ResetCooldownCommandRequest {
            target: user(99, "Taxpayer"),
            guild_id: 123,
            actor_id: 42,
        },
    );
    assert!(
        response
            .content
            .as_deref()
            .unwrap()
            .contains("did not have an active Tax Man fine cooldown")
    );
}

#[test]
fn test_tax_bankruptcy_add_calls_service() {
    let mut port = FakeEnforcement::new();
    port.bankruptcy_result = BankruptcyResult::Added {
        games: 3,
        previous_games: 0,
        penalty_games_remaining: 3,
    };
    let response = bankruptcy_command(
        &port,
        &tax_authorized(),
        BankruptcyCommandRequest {
            target: user(99, "Taxpayer"),
            guild_id: 123,
            actor_id: 42,
            action: BankruptcyAction::Add { games: 3 },
            reason: Some("manual review".to_owned()),
        },
    );
    assert_eq!(
        port.bankruptcy_requests.borrow().as_slice(),
        [BankruptcyRequest {
            user_id: 99,
            guild_id: 123,
            actor_id: 42,
            action: BankruptcyAction::Add { games: 3 },
            reason: Some("manual review".to_owned()),
        }]
    );
    let message = response.content.unwrap();
    assert!(message.contains("Added 3"));
    assert!(message.contains("0 -> 3"));
    assert!(response.ephemeral);
}

#[test]
fn test_tax_bankruptcy_remove_calls_service() {
    let mut port = FakeEnforcement::new();
    port.bankruptcy_result = BankruptcyResult::Removed {
        previous_games: 5,
        penalty_games_remaining: 0,
    };
    let response = bankruptcy_command(
        &port,
        &tax_authorized(),
        BankruptcyCommandRequest {
            target: user(99, "Taxpayer"),
            guild_id: 123,
            actor_id: 42,
            action: BankruptcyAction::Remove,
            reason: Some("appeal granted".to_owned()),
        },
    );
    assert_eq!(
        port.bankruptcy_requests.borrow().as_slice(),
        [BankruptcyRequest {
            user_id: 99,
            guild_id: 123,
            actor_id: 42,
            action: BankruptcyAction::Remove,
            reason: Some("appeal granted".to_owned()),
        }]
    );
    let message = response.content.unwrap();
    assert!(message.contains("Removed bankruptcy modifier"));
    assert!(message.contains("5 -> 0"));
    assert!(response.ephemeral);
}

#[test]
fn test_tax_vanity_reports_taxable_status_with_exemption_instructions() {
    let vanity = InMemoryVanityTax::default();
    vanity.refresh_member(123, 42, None);
    let response = vanity_command(
        Some(&vanity),
        &tax_man(),
        &[],
        123,
        42,
        &user(42, "NoNick"),
        None,
    );
    let message = response.content.unwrap();
    assert!(message.contains("10% vanity tax"));
    assert!(message.contains("Edit Server Profile"));
    assert!(response.ephemeral);
}

#[test]
fn test_tax_vanity_reports_exempt_status() {
    let vanity = InMemoryVanityTax::default();
    vanity.refresh_member(123, 99, Some("Real Name"));
    let response = vanity_command(
        Some(&vanity),
        &PermissionContext::new(99),
        &[],
        123,
        99,
        &user(99, "Nicked"),
        None,
    );
    assert!(response.content.unwrap().contains("exempt"));
}

#[test]
fn test_tax_vanity_reports_unknown_status_for_uncached_user() {
    let vanity = InMemoryVanityTax::default();
    vanity.refresh_member(123, 42, Some("Real Name"));
    let response = vanity_command(
        Some(&vanity),
        &PermissionContext::new(99),
        &[],
        123,
        99,
        &user(99, "NotCached"),
        None,
    );
    assert!(
        response
            .content
            .unwrap()
            .contains("not currently in the server member cache")
    );
}

#[test]
fn test_tax_vanity_allowlisted_admin_can_force_and_clear_manual_taxation() {
    let vanity = InMemoryVanityTax::default();
    vanity.refresh_member(123, 7, Some("Skater"));
    let permissions = PermissionContext::new(42);
    let target = user(7, "Skater");
    let enforce = vanity_command(
        Some(&vanity),
        &permissions,
        &[42],
        123,
        42,
        &target,
        Some(true),
    );
    assert_eq!(vanity.calculate_tax(7, 123, 500), 50);
    assert!(enforce.deferred && enforce.ephemeral);
    assert!(enforce.content.unwrap().contains("manually subject"));

    let status = vanity_command(
        Some(&vanity),
        &PermissionContext::new(7),
        &[42],
        123,
        7,
        &target,
        None,
    );
    assert!(status.content.unwrap().contains("admin override"));

    let clear = vanity_command(
        Some(&vanity),
        &permissions,
        &[42],
        123,
        42,
        &target,
        Some(false),
    );
    assert_eq!(vanity.calculate_tax(7, 123, 500), 0);
    assert!(clear.deferred && clear.ephemeral);
    assert!(clear.content.unwrap().contains("automatic nickname rule"));
}

#[test]
fn test_tax_vanity_clear_keeps_nicknameless_member_automatically_taxable() {
    let vanity = InMemoryVanityTax::default();
    vanity.refresh_member(123, 7, None);
    vanity
        .set_manual_taxation_atomic(ManualTaxationRequest {
            guild_id: 123,
            user_id: 7,
            enforced: true,
            actor_id: 42,
        })
        .unwrap();
    let response = vanity_command(
        Some(&vanity),
        &PermissionContext::new(42),
        &[42],
        123,
        42,
        &user(7, "NoNick"),
        Some(false),
    );
    assert!(!vanity.is_manually_taxed(123, 7));
    assert_eq!(
        vanity.eligibility_status(123, 7).unwrap(),
        VanityEligibility::Taxable
    );
    assert_eq!(vanity.calculate_tax(7, 123, 500), 50);
    assert!(
        response
            .content
            .unwrap()
            .contains("taxable because they have no server nickname")
    );
}

#[test]
fn test_tax_vanity_server_admin_can_override() {
    let vanity = InMemoryVanityTax::default();
    vanity.refresh_member(123, 7, Some("ANI"));
    let permissions =
        PermissionContext::new(42).with_guild_member_permissions(DiscordPermissions {
            administrator: true,
            manage_guild: true,
        });
    let response = vanity_command(
        Some(&vanity),
        &permissions,
        &[],
        123,
        42,
        &user(7, "ANI"),
        Some(true),
    );
    assert_eq!(vanity.calculate_tax(7, 123, 500), 50);
    assert!(response.deferred && response.ephemeral);
    assert!(response.content.unwrap().contains("manually subject"));
}

#[test]
fn test_tax_vanity_rejects_non_admin_override() {
    let vanity = InMemoryVanityTax::default();
    vanity.refresh_member(123, 7, Some("ANI"));
    let response = vanity_command(
        Some(&vanity),
        &PermissionContext::new(42),
        &[],
        123,
        42,
        &user(7, "ANI"),
        Some(true),
    );
    assert_eq!(vanity.calculate_tax(7, 123, 500), 0);
    assert!(response.content.unwrap().contains("Only admins"));
}

#[test]
fn test_tax_vanity_handles_missing_service() {
    let response = vanity_command::<InMemoryVanityTax>(
        None,
        &PermissionContext::new(42),
        &[],
        123,
        42,
        &user(42, "Anyone"),
        None,
    );
    assert!(response.ephemeral);
    assert!(response.content.unwrap().contains("not active"));
}

#[test]
fn test_tax_vanity_is_public_and_does_not_require_tax_man() {
    let vanity = InMemoryVanityTax::default();
    vanity.refresh_member(123, 42, None);
    let response = vanity_command(
        Some(&vanity),
        &PermissionContext::new(42),
        &[],
        123,
        42,
        &user(42, "NoNick"),
        None,
    );
    let message = response.content.unwrap();
    assert!(message.contains("vanity tax"));
    assert!(!message.contains("Only Tax Men"));
}
