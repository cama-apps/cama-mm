use cama_db::core_repositories::NewPlayer;
use tempfile::NamedTempFile;

use super::*;
use crate::test_support::copy_migrated_database as initialize_or_migrate;

/// The production defaults for every wheel settlement setting.
fn default_config() -> WheelSettlementConfig {
    WheelSettlementConfig {
        garnishment_rate: 1.0,
        bankruptcy_penalty_rate_per_game: 0.05,
        vanity_tax_rate: 0.2,
        low_priority_tax_rate: 0.10,
        minigame_jc_delta_scale: 1.0,
        wheel_target_ev: -27.5,
        wheel_bankrupt_target_ev: 12.0,
        wheel_golden_target_ev: -82.5,
    }
}

/// Test-only form of the daily event adjustment applied to a wedge.
fn apply_event_multiplier_to_wedge(
    wedge: &mut WheelWedge,
    win_multiplier: f64,
    loss_multiplier: f64,
) {
    let WheelValue::Numeric(value) = wedge.value else {
        return;
    };
    if value == 0 {
        return;
    }
    let adjusted = apply_gamba_event_multiplier(value, win_multiplier, loss_multiplier);
    wedge.value = WheelValue::Numeric(adjusted);
    wedge.label = adjusted.to_string();
}

#[test]
fn golden_wheel_member_snapshot_removes_departed_wealth_before_rank_selection() {
    let mut departed = Player::new("departed");
    departed.discord_id = Some(99);
    departed.jopacoin_balance = 10_000;
    let mut first = Player::new("first");
    first.discord_id = Some(1);
    first.jopacoin_balance = 900;
    let mut second = Player::new("second");
    second.discord_id = Some(2);
    second.jopacoin_balance = 800;
    let mut third = Player::new("third");
    third.discord_id = Some(3);
    third.jopacoin_balance = 700;
    let raw = vec![departed, first, second, third];
    let visible_ids = BTreeSet::from([1, 2, 3]);
    let visible = filter_visible_wheel_leaderboard(raw, Some(&visible_ids));
    assert_eq!(
        visible
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[test]
fn golden_wheel_member_snapshot_preserves_raw_order_when_unavailable() {
    let mut departed = Player::new("departed");
    departed.discord_id = Some(99);
    departed.jopacoin_balance = 10_000;
    let mut visible = Player::new("visible");
    visible.discord_id = Some(1);
    visible.jopacoin_balance = 900;
    let raw = vec![departed, visible];
    let result = filter_visible_wheel_leaderboard(raw, None);
    assert_eq!(result[0].discord_id, Some(99));
}

#[test]
fn golden_wheel_dividend_uses_visible_positive_wealth_snapshot() {
    let mut departed = Player::new("departed");
    departed.discord_id = Some(99);
    departed.jopacoin_balance = 100_000;
    let mut first = Player::new("first");
    first.discord_id = Some(1);
    first.jopacoin_balance = 2_500;
    let mut second = Player::new("second");
    second.discord_id = Some(2);
    second.jopacoin_balance = 1_500;
    let raw = vec![departed, first, second];
    let visible_ids = BTreeSet::from([1, 2]);
    assert_eq!(
        visible_wheel_positive_balance(&raw, Some(&visible_ids)),
        Some(4_000)
    );
}

#[test]
fn event_multiplier_changes_numeric_wedges_but_not_special_values() {
    let mut positive = WheelWedge {
        label: "10".to_owned(),
        value: WheelValue::Numeric(10),
        color: "#fff",
    };
    apply_event_multiplier_to_wedge(&mut positive, 1.5, 0.5);
    assert_eq!(positive.value, WheelValue::Numeric(15));
    assert_eq!(positive.label, "15");

    let mut zero = WheelWedge {
        label: "LOSE".to_owned(),
        value: WheelValue::Numeric(0),
        color: "#000",
    };
    apply_event_multiplier_to_wedge(&mut zero, 10.0, 0.0);
    assert_eq!(zero.value, WheelValue::Numeric(0));
    assert_eq!(zero.label, "LOSE");

    let mut mechanic = WheelWedge {
        label: "DISCOVER".to_owned(),
        value: WheelValue::Mechanic(WheelMechanic::Discover),
        color: "#000",
    };
    apply_event_multiplier_to_wedge(&mut mechanic, 2.0, 2.0);
    assert_eq!(
        mechanic.value,
        WheelValue::Mechanic(WheelMechanic::Discover)
    );
    assert_eq!(mechanic.label, "DISCOVER");
}

#[test]
fn gamba_special_minted_rewards_apply_event_after_central_scaling() {
    assert_eq!(scale_minted_gamba_reward(50, 0.5, 1.0, false), 25);
    assert_eq!(scale_minted_gamba_reward(100, 0.5, 1.0, false), 50);
}

#[test]
fn gamba_chain_reaction_scales_only_the_newly_minted_copy() {
    assert_eq!(scale_minted_gamba_reward(30, 0.5, 1.0, false), 15);
}

#[test]
fn gamba_dynamic_minted_rewards_apply_event_and_report_adjusted_amount() {
    assert_eq!(scale_minted_gamba_reward(20, 0.5, 1.0, false), 10);
    assert_eq!(scale_minted_gamba_reward(50, 0.5, 1.0, false), 25);
}

#[test]
fn gamba_heist_fallback_is_event_scaled() {
    assert_eq!(scale_minted_gamba_reward(20, 0.5, 1.0, false), 10);
}

#[test]
fn gamba_market_crash_fallback_is_event_scaled() {
    assert_eq!(scale_minted_gamba_reward(25, 0.5, 1.0, false), 12);
}

#[test]
fn gamba_hostile_takeover_fallback_is_event_scaled() {
    assert_eq!(scale_minted_gamba_reward(40, 0.5, 1.0, false), 20);
}

#[test]
fn hostile_result_notes_preserve_python_detail_fields() {
    let config = default_config();
    let lightning = HostileResolution {
        lightning_total: 27,
        lightning_count: 2,
        total: 27,
        count: 2,
        victims: vec![(11, 19), (12, 8)],
        shield_absorbed_total: 3,
        shielded_count: 1,
        ..HostileResolution::default()
    };
    let names = BTreeMap::from([(11, "Server Alice".to_owned())]);
    let note = hostile_result_note(WheelMechanic::LightningBolt, &lightning, &config, &names);
    assert!(note.contains("**2** player(s)"));
    assert!(note.contains("27"));
    assert!(note.contains("Server Alice"));
    assert!(note.contains("Unknown player"));
    assert!(!note.contains("bob"));
    assert!(note.contains("White Mana Shields"));

    let shell = HostileResolution {
        total: 12,
        victim_id: Some(13),
        victim_new_balance: Some(88),
        ..HostileResolution::default()
    };
    let note = hostile_result_note(
        WheelMechanic::RedShell,
        &shell,
        &config,
        &BTreeMap::from([(13, "Server Rival".to_owned())]),
    );
    assert!(note.contains("Server Rival"));
    assert!(note.contains("Victim's new balance: **88**"));
}

#[test]
fn hostile_mechanics_only_target_visible_guild_members() {
    // Python parity: every hostile victim pool reads the guild-member
    // visibility-filtered leaderboard, so a departed ex-member with the
    // largest balance can neither hold a rank nor be robbed.
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(1, "spinner", Some(42)))
        .expect("spinner");
    players
        .add(&NewPlayer::new(2, "member", Some(42)))
        .expect("member");
    players
        .add(&NewPlayer::new(99, "departed", Some(42)))
        .expect("departed");
    players
        .update_balance(1, Some(42), 100)
        .expect("spinner balance");
    players
        .update_balance(2, Some(42), 100)
        .expect("member balance");
    players
        .update_balance(99, Some(42), 1_000)
        .expect("departed balance");
    let config = default_config();
    let visible = BTreeSet::from([1_i64, 2]);

    let resolution = resolve_hostile_mechanic(
        database.path(),
        42,
        1,
        100,
        WheelMechanic::Heist,
        "heist-visibility",
        &config,
        &BTreeSet::new(),
        &BTreeSet::new(),
        1.0,
        Some(&visible),
        0,
        1_700_000_000,
    )
    .expect("resolve heist");

    assert!(resolution.total > 0, "the visible member must be robbed");
    let departed_balance = players
        .get_by_id(99, Some(42))
        .expect("read departed")
        .expect("departed row")
        .jopacoin_balance;
    assert_eq!(
        departed_balance, 1_000,
        "a departed ex-member must never be a hostile victim"
    );
    let member_balance = players
        .get_by_id(2, Some(42))
        .expect("read member")
        .expect("member row")
        .jopacoin_balance;
    assert!(
        member_balance < 100,
        "the visible member funds the heist (was {member_balance})"
    );
}

/// Records every wheel reminder a settlement schedules.
#[derive(Default)]
struct RecordingReminders(Mutex<Vec<(i64, i64, i64)>>);

impl WheelReminderPort for RecordingReminders {
    fn schedule_wheel(&self, user_id: i64, guild_id: i64, ready_at: i64) -> Result<(), String> {
        self.0
            .lock()
            .expect("reminder log")
            .push((user_id, guild_id, ready_at));
        Ok(())
    }
}

fn wheel_database(players: &[(i64, i64)]) -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let repository = PlayerRepository::new(database.path());
    for (discord_id, balance) in players {
        repository
            .add(&NewPlayer::new(*discord_id, "wheel player", Some(42)))
            .expect("player");
        repository
            .update_balance(*discord_id, Some(42), *balance)
            .expect("balance");
    }
    database
}

const NOW: i64 = 1_700_000_000;

#[test]
fn direct_spin_schedules_one_wheel_reminder_at_its_cooldown_expiry() {
    let database = wheel_database(&[(7, 100)]);
    let config = default_config();
    let reminders = RecordingReminders::default();
    let outcome = spin_wheel(&WheelSpinRequest {
        path: database.path(),
        guild_id: 42,
        user_id: 7,
        now: NOW,
        config: &config,
        event_win_multiplier: 1.0,
        event_loss_multiplier: 1.0,
        vanity_taxable_ids: &BTreeSet::new(),
        low_priority_taxable_ids: &BTreeSet::new(),
        admin_bypass: false,
        bonus_spin: false,
        visible_member_ids: None,
        player_names: &BTreeMap::new(),
        reminders: &reminders,
    })
    .expect("spin");
    let next_spin_at = match outcome {
        WheelSpinOutcome::Settled(settled) => settled.outcome.next_spin_at,
        WheelSpinOutcome::Exploded(exploded) => exploded.next_spin_at,
        other => panic!("an ordinary wheel settles directly: {other:?}"),
    };
    assert_eq!(
        reminders.0.into_inner().expect("reminder log"),
        vec![(7, 42, next_spin_at)]
    );

    // A second spin inside the cooldown is refused and schedules nothing.
    let refused = RecordingReminders::default();
    let outcome = spin_wheel(&WheelSpinRequest {
        path: database.path(),
        guild_id: 42,
        user_id: 7,
        now: NOW + 1,
        config: &config,
        event_win_multiplier: 1.0,
        event_loss_multiplier: 1.0,
        vanity_taxable_ids: &BTreeSet::new(),
        low_priority_taxable_ids: &BTreeSet::new(),
        admin_bypass: false,
        bonus_spin: false,
        visible_member_ids: None,
        player_names: &BTreeMap::new(),
        reminders: &refused,
    })
    .expect("refused spin");
    assert!(matches!(outcome, WheelSpinOutcome::Rejected(_)));
    assert!(refused.0.into_inner().expect("reminder log").is_empty());
}

#[test]
fn free_dig_bonus_spins_leave_the_wheel_reminder_alone() {
    let database = wheel_database(&[(7, 100)]);
    let reminders = RecordingReminders::default();
    spin_wheel(&WheelSpinRequest {
        path: database.path(),
        guild_id: 42,
        user_id: 7,
        now: NOW,
        config: &default_config(),
        event_win_multiplier: 1.0,
        event_loss_multiplier: 1.0,
        vanity_taxable_ids: &BTreeSet::new(),
        low_priority_taxable_ids: &BTreeSet::new(),
        admin_bypass: false,
        bonus_spin: true,
        visible_member_ids: None,
        player_names: &BTreeMap::new(),
        reminders: &reminders,
    })
    .expect("bonus spin");
    assert!(reminders.0.into_inner().expect("reminder log").is_empty());
}

#[allow(clippy::too_many_arguments)]
fn pending_request<'a>(
    path: &'a Path,
    config: &'a WheelSettlementConfig,
    effects: &'a ManaEffects,
    option: &'a WheelOption,
    visible_member_ids: Option<&'a BTreeSet<i64>>,
    names: &'a BTreeMap<i64, String>,
    reminders: &'a dyn WheelReminderPort,
    taxable: &'a BTreeSet<i64>,
) -> PendingWheelRequest<'a> {
    PendingWheelRequest {
        path,
        guild_id: 42,
        user_id: 7,
        balance_before: 100,
        kind: WheelKind::Bankrupt,
        effects,
        config,
        event_id: "pending-event",
        option,
        now: NOW,
        use_reroll: false,
        bonus_spin: false,
        last_regular_spin: None,
        event_win_multiplier: 1.0,
        event_loss_multiplier: 1.0,
        vanity_taxable_ids: taxable,
        low_priority_taxable_ids: taxable,
        visible_member_ids,
        player_names: names,
        reminders,
    }
}

#[test]
fn interactive_dividend_pays_from_the_visible_member_wealth() {
    // A departed ex-member's balance must not inflate the dividend, exactly as
    // on a direct spin.
    let database = wheel_database(&[(7, 100), (8, 4_000), (99, 1_000_000)]);
    let config = default_config();
    let effects = ManaEffects::default();
    let option = WheelOption {
        label: "DIVIDEND".to_owned(),
        value: WheelValue::Mechanic(WheelMechanic::Dividend),
        color: "#000000",
    };
    let visible = BTreeSet::from([7, 8]);
    let names = BTreeMap::new();
    let reminders = RecordingReminders::default();
    let taxable = BTreeSet::new();
    let outcome = resolve_pending_wheel(
        &pending_request(
            database.path(),
            &config,
            &effects,
            &option,
            Some(&visible),
            &names,
            &reminders,
            &taxable,
        ),
        &Mutex::default(),
    )
    .expect("interactive dividend");
    // 0.5% of the 4,100 visible JC is 20.
    assert_eq!(outcome.income.expect("dividend credit").gross, 20);
    assert_eq!(
        reminders.0.into_inner().expect("reminder log"),
        vec![(7, 42, outcome.next_spin_at)]
    );
}

#[test]
fn interactive_spin_history_records_the_insurance_flag_like_a_direct_spin() {
    let database = wheel_database(&[(7, 100)]);
    let config = default_config();
    let effects = ManaEffects::default();
    let option = WheelOption {
        label: "LOSE".to_owned(),
        value: WheelValue::Numeric(0),
        color: "#4a4a4a",
    };
    let names = BTreeMap::new();
    let taxable = BTreeSet::new();
    resolve_pending_wheel(
        &pending_request(
            database.path(),
            &config,
            &effects,
            &option,
            None,
            &names,
            &NoWheelReminders,
            &taxable,
        ),
        &Mutex::default(),
    )
    .expect("interactive spin");
    let metadata: String = rusqlite::Connection::open(database.path())
        .expect("open history")
        .query_row(
            "SELECT outcome_metadata FROM wheel_spins WHERE event_id='pending-event'",
            [],
            |row| row.get(0),
        )
        .expect("spin history");
    let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata JSON");
    assert_eq!(metadata["mana_insurance_activated"], json!(false));
    assert_eq!(metadata["interactive"], json!(true));
}
