//! Discord-independent Tax Man command policy and adapter ports.
//!
//! Python remains the live Discord and SQLite authority while this module is
//! exercised side by side.  Read models are typed, privileged commands gate
//! before touching their ports, and every enforcement mutation is a single
//! atomic port call so an eventual adapter cannot accidentally split a money
//! movement from its ledger/audit record.

use std::collections::BTreeMap;
use std::sync::Mutex;

use cama_domain::economy_scaling::DEFAULT_MINIGAME_JC_DELTA_SCALE;
use cama_domain::embed_safety::{
    DESCRIPTION_LIMIT, EmbedModel, FIELD_VALUE_LIMIT, add_lines_field, truncate_field,
};
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::permissions::{PermissionContext, has_admin_permission, has_tax_man_permission};

pub const LEDGER_DEFAULT_LIMIT: usize = 10;
pub const LEDGER_MAX_LIMIT: usize = 25;
pub const LEDGER_MAX_PAGE: usize = 10_000;
pub const LEDGER_VIEW_TIMEOUT_SECONDS: u64 = 300;
pub const PLAYER_LEDGER_DEFAULT_LIMIT: usize = 8;
pub const VANITY_TAX_BASIS_POINTS: i64 = 1_000;

pub const TAX_SUBCOMMANDS: [&str; 9] = [
    "audit",
    "event",
    "player",
    "ledger",
    "fine",
    "resetcooldown",
    "bankruptcy",
    "policy",
    "vanity",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaxEmbed {
    pub model: EmbedModel,
    pub thumbnail_url: Option<String>,
    pub color: u32,
}

impl TaxEmbed {
    #[must_use]
    pub fn text(&self) -> String {
        let mut parts = Vec::new();
        if let Some(title) = &self.model.title {
            parts.push(title.clone());
        }
        if let Some(description) = &self.model.description {
            parts.push(description.clone());
        }
        parts.extend(
            self.model
                .fields
                .iter()
                .map(|field| format!("{}\n{}", field.name, field.value)),
        );
        if let Some(footer) = &self.model.footer {
            parts.push(footer.clone());
        }
        parts.join("\n")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EconomyEffects {
    pub reward_multiplier: f64,
    pub gamba_win_multiplier: f64,
    pub gamba_loss_multiplier: f64,
    pub bet_payout_multiplier: f64,
    pub prediction_payout_multiplier: f64,
    /// Retained in the typed wire model. Legacy Python rows normalize this to
    /// one before presentation, so the public card intentionally omits it.
    pub prediction_depth_multiplier: f64,
    pub prediction_spread_ticks_delta: i32,
    pub reserve_voting_disabled: Option<bool>,
    pub reserve_burn_jc: i64,
    pub wallet_burn_jc: i64,
    pub reserve_release_jc: i64,
}

impl EconomyEffects {
    #[must_use]
    pub const fn neutral() -> Self {
        Self {
            reward_multiplier: 1.0,
            gamba_win_multiplier: 1.0,
            gamba_loss_multiplier: 1.0,
            bet_payout_multiplier: 1.0,
            prediction_payout_multiplier: 1.0,
            prediction_depth_multiplier: 1.0,
            prediction_spread_ticks_delta: 0,
            reserve_voting_disabled: None,
            reserve_burn_jc: 0,
            wallet_burn_jc: 0,
            reserve_release_jc: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EconomyDirection {
    Deflationary,
    Boon,
    Neutral,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EconomyEvent {
    pub name: String,
    pub severity: u8,
    pub direction: EconomyDirection,
    pub announcement: String,
    pub effects: EconomyEffects,
    pub forecast_flow_jc: i64,
    pub target_effect_jc: i64,
    pub expected_effect_jc: i64,
    pub direct_effect_jc: i64,
    pub ends_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EconomyPolicy {
    pub mode: String,
    pub target_annual_rate: f64,
    pub inflation_ceiling: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BalanceSheet {
    pub monetary_stock: i64,
    pub player_wallets: i64,
    pub average_wallet: f64,
    pub reserve_available: i64,
    pub reserve_locked: i64,
    pub prediction_open_cash: i64,
    pub wager_escrow: i64,
    pub player_count: usize,
    pub positive_wallets: i64,
    pub visible_debt: i64,
    pub reserve_next_match_pot: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonetaryTrend {
    pub elapsed_days: f64,
    pub period_rate: f64,
    pub daily_rate: f64,
    pub change_jc: i64,
    pub average_daily_change_jc: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlowIndicator {
    pub average_daily_net_jc: f64,
    pub average_daily_gross_jc: f64,
    pub daily_turnover_rate: f64,
    pub active_players: usize,
    pub active_player_rate: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Distribution {
    pub median_positive_wallet: f64,
    pub top_decile_share: f64,
    pub gini: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PolicyStatus {
    pub policy: EconomyPolicy,
    pub balance_sheet: BalanceSheet,
    pub annualized_30d: Option<f64>,
    pub annualized_90d: Option<f64>,
    pub monetary_trends: BTreeMap<&'static str, MonetaryTrend>,
    pub flow_indicators: BTreeMap<&'static str, FlowIndicator>,
    pub distribution: Distribution,
    pub event: Option<EconomyEvent>,
}

#[must_use]
pub fn build_public_event_embed(status: &PolicyStatus, icon_url: Option<&str>) -> TaxEmbed {
    let Some(event) = &status.event else {
        return TaxEmbed {
            model: EmbedModel {
                title: Some("🌤️ The Economy Is Between Spells".to_owned()),
                description: Some(
                    "No server-wide economic edict is active. The next decree is scheduled for **10 AM Pacific**."
                        .to_owned(),
                ),
                ..EmbedModel::default()
            },
            thumbnail_url: None,
            color: 0x7F8C8D,
        };
    };

    let severity = event.severity.clamp(1, 5);
    let level = ["I", "II", "III", "IV", "V"][usize::from(severity - 1)];
    let (icon, color, edict) = match event.direction {
        EconomyDirection::Deflationary => {
            ("🌑", 0xD94B4B, "A deflationary edict grips the server.")
        }
        EconomyDirection::Boon => ("✨", 0x43B581, "A boon washes over the server."),
        EconomyDirection::Neutral => ("🕰️", 0x7F8C8D, "The market bends without changing course."),
    };
    let flavor = event
        .announcement
        .trim()
        .lines()
        .next()
        .filter(|line| !line.is_empty())
        .unwrap_or(edict);
    let expiry = event.ends_at.map_or_else(
        || "Active until the next 10 AM Pacific rollover.".to_owned(),
        |timestamp| format!("Active until <t:{timestamp}:R>."),
    );

    let mut model = EmbedModel {
        title: Some(format!("{icon} {} — Level {level}", event.name)),
        description: Some(format!("*{flavor}*\n\n**{edict}** {expiry}")),
        footer: Some(
            "The treasury watches. Bands follow the rolling 7-day change around a 2% annual target."
                .to_owned(),
        ),
        ..EmbedModel::default()
    };
    model.add_field(
        "The Spell Takes Effect",
        event_effect_lines(event).join("\n"),
        false,
    );
    model.add_field(
        "Immediate Impact",
        immediate_effect_text(&event.effects),
        false,
    );
    TaxEmbed {
        model,
        thumbnail_url: icon_url.map(str::to_owned),
        color,
    }
}

fn relative_effect(multiplier: f64, lower: &str, higher: &str) -> Option<String> {
    let change = (multiplier - 1.0) * 100.0;
    if change.abs() < 0.05 {
        return None;
    }
    let magnitude = (change.abs() * 10.0).round() / 10.0;
    let amount = if magnitude.fract().abs() < f64::EPSILON {
        format!("{magnitude:.0}%")
    } else {
        format!("{magnitude:.1}%")
    };
    Some(format!(
        "**{amount} {}**",
        if change < 0.0 { lower } else { higher }
    ))
}

fn event_effect_lines(event: &EconomyEvent) -> Vec<String> {
    let effects = event.effects;
    let mut lines = Vec::new();
    if let Some(effect) = relative_effect(effects.reward_multiplier, "lower", "higher") {
        lines.push(format!("🎁 Generated rewards are {effect}."));
    }
    let mut gamba = Vec::new();
    if let Some(effect) = relative_effect(effects.gamba_win_multiplier, "lower", "higher") {
        gamba.push(format!("wins {effect}"));
    }
    if let Some(effect) = relative_effect(effects.gamba_loss_multiplier, "softer", "harsher") {
        gamba.push(format!("losses {effect}"));
    }
    if !gamba.is_empty() {
        lines.push(format!("🎰 Gamba: {}.", gamba.join(", ")));
    }
    if let Some(effect) = relative_effect(effects.bet_payout_multiplier, "lower", "higher") {
        lines.push(format!("⚔️ Match-bet payouts are {effect}."));
    }
    let mut prediction = Vec::new();
    if let Some(effect) = relative_effect(effects.prediction_payout_multiplier, "lower", "higher") {
        prediction.push(format!("resolution payouts {effect}"));
    }
    let spread = effects.prediction_spread_ticks_delta;
    if spread != 0 {
        prediction.push(format!(
            "spreads **{} ticks {}**",
            spread.unsigned_abs(),
            if spread > 0 { "wider" } else { "tighter" }
        ));
    }
    if !prediction.is_empty() {
        lines.push(format!("📈 Prediction markets: {}.", prediction.join(", ")));
    }
    let voting_disabled = effects.reserve_voting_disabled.unwrap_or(
        event.direction == EconomyDirection::Deflationary && event.severity.clamp(1, 5) >= 3,
    );
    if voting_disabled {
        lines.push("🏛️ Jopacoin Reserve allocation voting is suspended.".to_owned());
    }
    if lines.is_empty() {
        lines.push("⚖️ Payouts and market conditions remain at their normal strength.".to_owned());
    }
    lines
}

fn immediate_effect_text(effects: &EconomyEffects) -> String {
    let mut actions = Vec::new();
    if effects.reserve_burn_jc != 0 {
        actions.push(format!(
            "**{} JC** burned from the Jopa Reserve",
            grouped(effects.reserve_burn_jc)
        ));
    }
    if effects.wallet_burn_jc != 0 {
        actions.push(format!(
            "**{} JC** burned from positive wallets",
            grouped(effects.wallet_burn_jc)
        ));
    }
    if effects.reserve_release_jc != 0 {
        actions.push(format!(
            "**{} JC** redistributed from the Jopa Reserve",
            grouped(effects.reserve_release_jc)
        ));
    }
    if actions.is_empty() {
        "No JC moved when this spell activated. Its pressure arrives through adjusted payouts and liquidity as activity settles."
            .to_owned()
    } else {
        format!("The spell took immediate hold: {}.", actions.join("; "))
    }
}

#[must_use]
pub fn build_policy_embed(status: &PolicyStatus) -> TaxEmbed {
    let policy = &status.policy;
    let sheet = &status.balance_sheet;
    let mode = title_case(&policy.mode.replace('_', " "));
    let mut model = EmbedModel {
        title: Some("Jopacoin Monetary Policy".to_owned()),
        description: Some(format!(
            "**Mode:** {mode}\n**Annual target:** {:+.2}%\n**Inflation ceiling:** {:.2}%\n**Generated payout baseline:** {:.0}%\n**Reserve voting:** {}",
            policy.target_annual_rate * 100.0,
            policy.inflation_ceiling * 100.0,
            DEFAULT_MINIGAME_JC_DELTA_SCALE * 100.0,
            if mode == "Recovery" { "Paused" } else { "Enabled" }
        )),
        footer: Some(
            "Supply growth is a best-effort inflation proxy, not a price index. Windows use the nearest daily stock snapshot; turnover is gross ledger balance movement."
                .to_owned(),
        ),
        ..EmbedModel::default()
    };
    model.add_field(
        "Reconciled stock",
        format!(
            "Total: **{} JC**\nWallets: **{} JC** (avg **{:.2}**)\nPositive wallets: **{} JC** · visible debt: **{} JC**\nReserve: **{} available** + **{} locked**\nNext-match pot: **{} JC**\nPrediction cash: **{} JC**\nWager escrow: **{} JC**",
            grouped(sheet.monetary_stock),
            grouped(sheet.player_wallets),
            sheet.average_wallet,
            grouped(sheet.positive_wallets),
            grouped(sheet.visible_debt),
            grouped(sheet.reserve_available),
            grouped(sheet.reserve_locked),
            grouped(sheet.reserve_next_match_pot),
            grouped(sheet.prediction_open_cash),
            grouped(sheet.wager_escrow),
        ),
        false,
    );

    let mut trend_lines = vec![
        format_supply_trend("Live ~24h", status.monetary_trends.get("1d")),
        format_supply_trend("Rolling 3d", status.monetary_trends.get("3d")),
        format_supply_trend("Rolling 7d", status.monetary_trends.get("7d")),
    ];
    if let Some(momentum) = format_supply_momentum(&status.monetary_trends) {
        trend_lines.push(momentum);
    }
    trend_lines.push(format_optional_annualized(
        "30-day annualized",
        status.annualized_30d,
    ));
    trend_lines.push(format_optional_annualized(
        "90-day annualized",
        status.annualized_90d,
    ));
    model.add_field(
        "Money-supply inflation proxy",
        trend_lines.join("\n"),
        false,
    );

    let mut flow_lines = vec![
        format_flow_indicator("3d", status.flow_indicators.get("3d")),
        format_flow_indicator("7d", status.flow_indicators.get("7d")),
    ];
    if let Some(flow) = status.flow_indicators.get("7d") {
        flow_lines.push(format!(
            "7d participation: **{}/{} players** ({:.1}%)",
            grouped_usize(flow.active_players),
            grouped_usize(sheet.player_count),
            flow.active_player_rate * 100.0
        ));
    }
    model.add_field("Flows & activity", flow_lines.join("\n"), false);

    let committed = sheet.reserve_locked
        + sheet.reserve_next_match_pot
        + sheet.prediction_open_cash
        + sheet.wager_escrow;
    let per_player = ratio_or_na(
        sheet.monetary_stock as f64,
        sheet.player_count as f64,
        2,
        " JC",
    );
    let debt_ratio = percent_or_na(sheet.visible_debt as f64, sheet.positive_wallets as f64, 1);
    let reserve_ratio = percent_or_na(
        sheet.reserve_available as f64,
        sheet.positive_wallets as f64,
        1,
    );
    let committed_ratio = percent_or_na(committed as f64, sheet.monetary_stock as f64, 1);
    model.add_field(
        "Player-economy health",
        format!(
            "Supply per registered player: **{per_player}**\nVisible debt / positive wallets: **{debt_ratio}**\nAvailable Reserve / positive wallets: **{reserve_ratio}**\nCommitted or escrowed supply: **{committed_ratio}**\nMedian positive wallet: **{:.2} JC**\nPositive-wallet top 10% share: **{:.1}%** · positive-wallet Gini: **{:.3}**",
            status.distribution.median_positive_wallet,
            status.distribution.top_decile_share * 100.0,
            status.distribution.gini,
        ),
        false,
    );

    if let Some(event) = &status.event {
        let immediate = if event.direct_effect_jc == 0 {
            "**None** — this event works through adjusted outcomes".to_owned()
        } else {
            format!("**{} JC**", grouped_signed(event.direct_effect_jc))
        };
        model.add_field(
            format!("Active event: {} (Level {})", event.name, event.severity),
            format!(
                "Observed 7-day stock movement: **{} JC/day**\nToday's target correction: **{} JC**\nProjected daily impact: **{} JC**\nImmediate supply change: {immediate}",
                grouped_signed(event.forecast_flow_jc),
                grouped_signed(event.target_effect_jc),
                grouped_signed(event.expected_effect_jc),
            ),
            false,
        );
    } else {
        model.add_field("Active event", "No event has activated today.", false);
    }

    TaxEmbed {
        model,
        thumbnail_url: None,
        color: if mode == "Recovery" {
            0xD94B4B
        } else {
            0x43B581
        },
    }
}

fn format_supply_trend(label: &str, trend: Option<&MonetaryTrend>) -> String {
    let Some(trend) = trend else {
        return format!("{label}: **collecting data**");
    };
    let arrow = if trend.daily_rate > 0.0 {
        "↗"
    } else if trend.daily_rate < 0.0 {
        "↘"
    } else {
        "→"
    };
    format!(
        "{label}: **{:+.2}%** ({} JC) {arrow} **{:+.2}%/day**, **{} JC/day** over {:.1}d",
        trend.period_rate * 100.0,
        grouped_signed(trend.change_jc),
        trend.daily_rate * 100.0,
        grouped_signed(trend.average_daily_change_jc.round() as i64),
        trend.elapsed_days,
    )
}

fn format_supply_momentum(trends: &BTreeMap<&'static str, MonetaryTrend>) -> Option<String> {
    let three_day = trends.get("3d")?;
    let seven_day = trends.get("7d")?;
    let difference = three_day.daily_rate - seven_day.daily_rate;
    let label = if difference.abs() <= 0.0001 {
        "steady"
    } else if difference > 0.0 {
        "heating"
    } else {
        "cooling"
    };
    Some(format!(
        "Momentum: **{label}** (3d pace vs 7d: **{:+.2}%/day**)",
        difference * 100.0
    ))
}

fn format_flow_indicator(label: &str, flow: Option<&FlowIndicator>) -> String {
    let Some(flow) = flow else {
        return format!("{label}: **collecting data**");
    };
    format!(
        "{label}: net **{} JC/day** · ledger movement **{} JC/day** · turnover **{:.2}%/day**",
        grouped_signed(flow.average_daily_net_jc.round() as i64),
        grouped(flow.average_daily_gross_jc.round() as i64),
        flow.daily_turnover_rate * 100.0,
    )
}

fn format_optional_annualized(label: &str, rate: Option<f64>) -> String {
    rate.map_or_else(
        || format!("{label}: collecting data"),
        |rate| format!("{label}: **{:+.2}%**", rate * 100.0),
    )
}

fn ratio_or_na(numerator: f64, denominator: f64, precision: usize, suffix: &str) -> String {
    if denominator == 0.0 {
        "n/a".to_owned()
    } else {
        format!("{:.*}{suffix}", precision, numerator / denominator)
    }
}

fn percent_or_na(numerator: f64, denominator: f64, precision: usize) -> String {
    if denominator == 0.0 {
        "n/a".to_owned()
    } else {
        format!("{:.*}%", precision, numerator / denominator * 100.0)
    }
}

fn title_case(value: &str) -> String {
    value
        .split_whitespace()
        .map(|word| {
            let mut characters = word.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + characters.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRef {
    pub id: u64,
    pub name: String,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LedgerAccount {
    Player(u64),
    Reserve,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerRow {
    pub ledger_id: i64,
    pub guild_id: u64,
    pub account: LedgerAccount,
    pub delta: i64,
    pub balance_before: i64,
    pub balance_after: i64,
    pub source: String,
    pub actor_id: Option<u64>,
    pub related_type: Option<String>,
    pub related_id: Option<String>,
    pub reason: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PredictionSummary {
    pub cost_basis: i64,
    pub expected_payout: i64,
    pub ev: i64,
    pub max_payout: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PredictionPosition {
    pub prediction_id: i64,
    pub current_price: i64,
    pub question: String,
    pub yes_contracts: i64,
    pub yes_cost_basis: i64,
    pub no_contracts: i64,
    pub no_cost_basis: i64,
    pub ev: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PredictionExposure {
    pub summary: PredictionSummary,
    pub positions: Vec<PredictionPosition>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlayerSnapshot {
    pub balance: i64,
    pub visible_debt: i64,
    pub loan_principal: i64,
    pub loan_fee: i64,
    pub loan_total: i64,
    pub total_loans_taken: i64,
    pub bankruptcy_count: i64,
    pub penalty_games_remaining: i64,
    pub dark_bargain_count: i64,
    pub dark_bargain_due: i64,
    pub effective_obligations: i64,
    pub prediction_exposure: PredictionExposure,
    pub recent_ledger: Vec<LedgerRow>,
}

#[must_use]
pub fn ledger_total_pages(total_entries: usize, limit: usize) -> usize {
    let limit = limit.max(1);
    total_entries.div_ceil(limit).max(1)
}

#[must_use]
pub fn clamp_ledger_page(page: usize, limit: usize, total_entries: usize) -> usize {
    page.max(1).min(ledger_total_pages(total_entries, limit))
}

#[must_use]
pub fn ledger_offset(page: usize, limit: usize) -> usize {
    (page.max(1) - 1).saturating_mul(limit.max(1))
}

#[must_use]
pub fn ledger_footer_text(page: usize, total_entries: usize, limit: usize) -> String {
    let page = clamp_ledger_page(page, limit, total_entries);
    let total_pages = ledger_total_pages(total_entries, limit);
    if total_entries == 0 {
        return "Tax Man ledger | Page 1/1 | 0 entries".to_owned();
    }
    let first_entry = ledger_offset(page, limit) + 1;
    let last_entry = total_entries.min(page.saturating_mul(limit.max(1)));
    format!(
        "Tax Man ledger | Page {page}/{total_pages} | Entries {}-{} of {}",
        grouped_usize(first_entry),
        grouped_usize(last_entry),
        grouped_usize(total_entries)
    )
}

fn player_ledger_footer_text(page: usize, total_entries: usize, limit: usize) -> String {
    format!(
        "Tax Man audit only | Recent ledger {}",
        ledger_footer_text(page, total_entries, limit)
            .strip_prefix("Tax Man ledger | ")
            .unwrap_or_default()
    )
}

#[must_use]
pub fn format_ledger_rows(rows: &[LedgerRow]) -> String {
    format_ledger_lines(rows, 25).join("\n")
}

fn format_ledger_lines(rows: &[LedgerRow], limit: usize) -> Vec<String> {
    if rows.is_empty() {
        return vec!["No ledger entries yet.".to_owned()];
    }
    rows.iter()
        .take(limit)
        .map(|row| {
            let account = match row.account {
                LedgerAccount::Player(account_id) => format!("<@{account_id}>"),
                LedgerAccount::Reserve => "Jopacoin Reserve".to_owned(),
            };
            format!(
                "<t:{}:R> - {account}: {} - {} -> {}",
                row.created_at,
                format_signed_jc(row.delta),
                ledger_detail(row),
                format_jc(row.balance_after),
            )
        })
        .collect()
}

fn ledger_detail(row: &LedgerRow) -> String {
    if let Some(reason) = row
        .reason
        .as_deref()
        .map(normalize_whitespace)
        .filter(|reason| !reason.is_empty())
    {
        return reason;
    }
    let label = match row.source.as_str() {
        "balance_update" => "balance adjustment".to_owned(),
        "player_insert" => "registration starting balance".to_owned(),
        "nonprofit_insert" => "Jopacoin Reserve created".to_owned(),
        "nonprofit_update" => "Jopacoin Reserve update".to_owned(),
        "ledger_backfill" => "opening balance backfill".to_owned(),
        "dig" => "dig balance change".to_owned(),
        "gamba" => "gamba wheel balance change".to_owned(),
        source => source.replace('_', " "),
    };
    let related_type = row
        .related_type
        .as_deref()
        .map(normalize_whitespace)
        .unwrap_or_default();
    let related_id = row
        .related_id
        .as_deref()
        .map(normalize_whitespace)
        .unwrap_or_default();
    match (related_type.is_empty(), related_id.is_empty()) {
        (false, false) => format!("{label} ({related_type} #{related_id})"),
        (false, true) => format!("{label} ({related_type})"),
        _ => label,
    }
}

#[must_use]
pub fn build_ledger_embed(
    rows: &[LedgerRow],
    user: Option<&UserRef>,
    page: usize,
    total_entries: Option<usize>,
    limit: usize,
) -> TaxEmbed {
    let title = user.map_or_else(
        || "Central Economy Ledger".to_owned(),
        |user| format!("Central Economy Ledger - {}", user.display_name),
    );
    let description = if rows.is_empty() && total_entries.is_some_and(|total| total > 0) {
        "No ledger entries on this page.".to_owned()
    } else {
        format_ledger_rows(rows)
    };
    TaxEmbed {
        model: EmbedModel {
            title: Some(title),
            description: Some(truncate_field(&description, DESCRIPTION_LIMIT)),
            footer: total_entries.map(|total| ledger_footer_text(page, total, limit)),
            ..EmbedModel::default()
        },
        thumbnail_url: None,
        color: 0x5865F2,
    }
}

#[must_use]
pub fn build_player_embed(
    user: &UserRef,
    snapshot: &PlayerSnapshot,
    ledger_page: usize,
    total_ledger_entries: Option<usize>,
    ledger_limit: usize,
) -> TaxEmbed {
    let mut model = EmbedModel {
        title: Some(format!("Tax Man Player Audit - {}", user.display_name)),
        footer: Some(total_ledger_entries.map_or_else(
            || "Tax Man audit only".to_owned(),
            |total| player_ledger_footer_text(ledger_page, total, ledger_limit),
        )),
        ..EmbedModel::default()
    };
    model.add_field(
        "Balance",
        format!(
            "Current: {}\nVisible debt: {}",
            format_jc(snapshot.balance),
            format_jc(snapshot.visible_debt)
        ),
        true,
    );
    model.add_field(
        "Loans",
        format!(
            "Principal: {}\nFee: {}\nTotal owed: {}\nTaken: {}",
            format_jc(snapshot.loan_principal),
            format_jc(snapshot.loan_fee),
            format_jc(snapshot.loan_total),
            grouped(snapshot.total_loans_taken),
        ),
        true,
    );
    model.add_field(
        "Duration Effects",
        format!(
            "Bankruptcies: {}\nPenalty games: {}\nDark Bargains: {}\nDark Bargain due: {}",
            grouped(snapshot.bankruptcy_count),
            grouped(snapshot.penalty_games_remaining),
            grouped(snapshot.dark_bargain_count),
            format_jc(snapshot.dark_bargain_due),
        ),
        true,
    );
    model.add_field(
        "Total Obligations",
        format_jc(snapshot.effective_obligations),
        false,
    );
    model.add_field(
        "Prediction Positions",
        format_prediction_positions(&snapshot.prediction_exposure),
        false,
    );
    add_lines_field(
        &mut model,
        "Recent Ledger",
        &format_ledger_lines(&snapshot.recent_ledger, 25),
        false,
        FIELD_VALUE_LIMIT,
    );
    TaxEmbed {
        model,
        thumbnail_url: None,
        color: 0xB8860B,
    }
}

fn format_prediction_positions(exposure: &PredictionExposure) -> String {
    if exposure.positions.is_empty() {
        return "No open prediction positions.".to_owned();
    }
    let summary = &exposure.summary;
    let mut lines = vec![format!(
        "Cost basis: {} | Expected payout: {} | EV: {} | Max payout: {}",
        format_jc(summary.cost_basis),
        format_jc(summary.expected_payout),
        format_signed_jc(summary.ev),
        format_jc(summary.max_payout),
    )];
    lines.extend(exposure.positions.iter().take(5).map(|position| {
        format!(
            "#{} `{}%` {}\n  YES {} ({}) / NO {} ({}); EV {}",
            position.prediction_id,
            position.current_price,
            short_text(&position.question, 48),
            grouped(position.yes_contracts),
            format_jc(position.yes_cost_basis),
            grouped(position.no_contracts),
            format_jc(position.no_cost_basis),
            format_signed_jc(position.ev),
        )
    }));
    lines.join("\n")
}

fn short_text(value: &str, limit: usize) -> String {
    let value = normalize_whitespace(value);
    if value.chars().count() <= limit {
        value
    } else {
        let mut result = value
            .chars()
            .take(limit.saturating_sub(1))
            .collect::<String>();
        result.push_str("...");
        result
    }
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_jc(amount: i64) -> String {
    format!("{} {JOPACOIN_EMOTE}", grouped(amount))
}

fn format_signed_jc(amount: i64) -> String {
    format!("{} {JOPACOIN_EMOTE}", grouped_signed(amount))
}

fn grouped_signed(amount: i64) -> String {
    if amount > 0 {
        format!("+{}", grouped(amount))
    } else {
        grouped(amount)
    }
}

fn grouped_usize(amount: usize) -> String {
    group_digits(&amount.to_string())
}

fn grouped(amount: i64) -> String {
    let negative = amount < 0;
    let digits = amount.unsigned_abs().to_string();
    let result = group_digits(&digits);
    if negative {
        format!("-{result}")
    } else {
        result
    }
}

fn group_digits(digits: &str) -> String {
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    let first = digits.len() % 3;
    if first != 0 {
        output.push_str(&digits[..first]);
    }
    for chunk in digits.as_bytes()[first..].chunks(3) {
        if !output.is_empty() {
            output.push(',');
        }
        output.push_str(std::str::from_utf8(chunk).expect("decimal digits are UTF-8"));
    }
    output
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandResponse {
    pub ephemeral: bool,
    pub deferred: bool,
    pub content: Option<String>,
    pub embed: Option<TaxEmbed>,
}

impl CommandResponse {
    fn message(content: impl Into<String>, ephemeral: bool) -> Self {
        Self {
            ephemeral,
            deferred: false,
            content: Some(content.into()),
            embed: None,
        }
    }

    fn followup(content: impl Into<String>, ephemeral: bool) -> Self {
        Self {
            ephemeral,
            deferred: true,
            content: Some(content.into()),
            embed: None,
        }
    }

    fn embed(embed: TaxEmbed, ephemeral: bool) -> Self {
        Self {
            ephemeral,
            deferred: true,
            content: None,
            embed: Some(embed),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TaxCommandAuthorization<'a> {
    pub permissions: PermissionContext,
    pub admin_user_ids: &'a [u64],
    pub tax_man_user_ids: &'a [u64],
}

fn tax_man_denial(authorization: &TaxCommandAuthorization<'_>) -> Option<CommandResponse> {
    if has_tax_man_permission(
        &authorization.permissions,
        authorization.admin_user_ids,
        authorization.tax_man_user_ids,
    ) {
        None
    } else {
        Some(CommandResponse::message(
            "Only Tax Men can use this command.",
            true,
        ))
    }
}

pub trait EconomyStatusPort {
    type Error;

    fn get_policy_status(&self, guild_id: u64) -> Result<PolicyStatus, Self::Error>;
}

pub trait SpellIconPort {
    type Error;

    fn get_ability_icon_url(&self, event_name: &str) -> Result<Option<String>, Self::Error>;
}

/// Public command: permission policy is deliberately absent.
#[must_use]
pub fn event_command<S: EconomyStatusPort, I: SpellIconPort>(
    guild_id: u64,
    economy: Option<&S>,
    icons: &I,
) -> CommandResponse {
    let Some(economy) = economy else {
        return CommandResponse::followup("The daily economy controller is not configured.", false);
    };
    let Ok(status) = economy.get_policy_status(guild_id) else {
        return CommandResponse::followup(
            "The current economic edict is obscured. Try again shortly.",
            false,
        );
    };
    let icon = status
        .event
        .as_ref()
        .and_then(|event| icons.get_ability_icon_url(&event.name).ok().flatten());
    CommandResponse::embed(build_public_event_embed(&status, icon.as_deref()), false)
}

#[must_use]
pub fn policy_command<S: EconomyStatusPort>(
    guild_id: u64,
    authorization: &TaxCommandAuthorization<'_>,
    economy: Option<&S>,
) -> CommandResponse {
    if let Some(response) = tax_man_denial(authorization) {
        return response;
    }
    let Some(economy) = economy else {
        return CommandResponse::followup("The daily economy controller is not configured.", true);
    };
    match economy.get_policy_status(guild_id) {
        Ok(status) => CommandResponse::embed(build_policy_embed(&status), true),
        Err(_) => {
            CommandResponse::followup("Couldn't load the monetary-policy status right now.", true)
        }
    }
}

pub trait LedgerReadPort {
    type Error;

    fn count_ledger_entries(
        &self,
        guild_id: u64,
        user_id: Option<u64>,
    ) -> Result<usize, Self::Error>;

    fn get_recent_ledger(
        &self,
        guild_id: u64,
        limit: usize,
        offset: usize,
        user_id: Option<u64>,
    ) -> Result<Vec<LedgerRow>, Self::Error>;
}

pub trait PlayerAuditReadPort: LedgerReadPort {
    fn get_player_snapshot(
        &self,
        user_id: u64,
        guild_id: u64,
        ledger_limit: usize,
        ledger_offset: usize,
    ) -> Result<PlayerSnapshot, Self::Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaginationButtons {
    pub previous_disabled: bool,
    pub next_disabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageInteractionResult {
    pub deferred: bool,
    pub edited_embed: Option<TaxEmbed>,
    pub ephemeral_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaxLedgerView {
    pub guild_id: u64,
    pub requester_id: u64,
    pub user: Option<UserRef>,
    pub limit: usize,
    pub current_page: usize,
    pub total_entries: usize,
    pub total_pages: usize,
    pub buttons: PaginationButtons,
}

impl TaxLedgerView {
    #[must_use]
    pub fn new(
        guild_id: u64,
        requester_id: u64,
        user: Option<UserRef>,
        limit: usize,
        current_page: usize,
        total_entries: usize,
    ) -> Self {
        let current_page = clamp_ledger_page(current_page, limit, total_entries);
        let total_pages = ledger_total_pages(total_entries, limit);
        Self {
            guild_id,
            requester_id,
            user,
            limit,
            current_page,
            total_entries,
            total_pages,
            buttons: buttons(current_page, total_pages),
        }
    }

    pub fn interaction_denial(&self, user_id: u64) -> Option<CommandResponse> {
        if user_id == self.requester_id {
            None
        } else {
            Some(CommandResponse::message(
                "This ledger view belongs to another Tax Man.",
                true,
            ))
        }
    }

    /// Acknowledgement happens conceptually before either query. State is
    /// committed only after count and page reads both succeed.
    pub fn show_page<P: LedgerReadPort>(
        &mut self,
        port: &P,
        requested_page: usize,
    ) -> PageInteractionResult {
        let user_id = self.user.as_ref().map(|user| user.id);
        let Ok(total_entries) = port.count_ledger_entries(self.guild_id, user_id) else {
            return page_error("Couldn't load that ledger page right now.");
        };
        let current_page = clamp_ledger_page(requested_page, self.limit, total_entries);
        let Ok(rows) = port.get_recent_ledger(
            self.guild_id,
            self.limit,
            ledger_offset(current_page, self.limit),
            user_id,
        ) else {
            return page_error("Couldn't load that ledger page right now.");
        };
        let total_pages = ledger_total_pages(total_entries, self.limit);
        let embed = build_ledger_embed(
            &rows,
            self.user.as_ref(),
            current_page,
            Some(total_entries),
            self.limit,
        );
        self.total_entries = total_entries;
        self.total_pages = total_pages;
        self.current_page = current_page;
        self.buttons = buttons(current_page, total_pages);
        PageInteractionResult {
            deferred: true,
            edited_embed: Some(embed),
            ephemeral_error: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaxPlayerLedgerView {
    pub guild_id: u64,
    pub requester_id: u64,
    pub user: UserRef,
    pub limit: usize,
    pub current_page: usize,
    pub total_entries: usize,
    pub total_pages: usize,
    pub buttons: PaginationButtons,
}

impl TaxPlayerLedgerView {
    #[must_use]
    pub fn new(
        guild_id: u64,
        requester_id: u64,
        user: UserRef,
        limit: usize,
        current_page: usize,
        total_entries: usize,
    ) -> Self {
        let current_page = clamp_ledger_page(current_page, limit, total_entries);
        let total_pages = ledger_total_pages(total_entries, limit);
        Self {
            guild_id,
            requester_id,
            user,
            limit,
            current_page,
            total_entries,
            total_pages,
            buttons: buttons(current_page, total_pages),
        }
    }

    pub fn interaction_denial(&self, user_id: u64) -> Option<CommandResponse> {
        if user_id == self.requester_id {
            None
        } else {
            Some(CommandResponse::message(
                "This player audit view belongs to another Tax Man.",
                true,
            ))
        }
    }

    pub fn show_page<P: PlayerAuditReadPort>(
        &mut self,
        port: &P,
        requested_page: usize,
    ) -> PageInteractionResult {
        let Ok(total_entries) = port.count_ledger_entries(self.guild_id, Some(self.user.id)) else {
            return page_error("Couldn't load that player ledger page right now.");
        };
        let current_page = clamp_ledger_page(requested_page, self.limit, total_entries);
        let Ok(snapshot) = port.get_player_snapshot(
            self.user.id,
            self.guild_id,
            self.limit,
            ledger_offset(current_page, self.limit),
        ) else {
            return page_error("Couldn't load that player ledger page right now.");
        };
        let total_pages = ledger_total_pages(total_entries, self.limit);
        let embed = build_player_embed(
            &self.user,
            &snapshot,
            current_page,
            Some(total_entries),
            self.limit,
        );
        self.total_entries = total_entries;
        self.total_pages = total_pages;
        self.current_page = current_page;
        self.buttons = buttons(current_page, total_pages);
        PageInteractionResult {
            deferred: true,
            edited_embed: Some(embed),
            ephemeral_error: None,
        }
    }
}

fn buttons(current_page: usize, total_pages: usize) -> PaginationButtons {
    PaginationButtons {
        previous_disabled: current_page <= 1,
        next_disabled: current_page >= total_pages,
    }
}

fn page_error(message: &str) -> PageInteractionResult {
    PageInteractionResult {
        deferred: true,
        edited_embed: None,
        ephemeral_error: Some(message.to_owned()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerCommandOutput {
    pub response: CommandResponse,
    pub view: Option<TaxLedgerView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerCommandRequest {
    pub guild_id: u64,
    pub requester_id: u64,
    pub user: Option<UserRef>,
    pub page: usize,
    pub limit: usize,
}

#[must_use]
pub fn ledger_command<P: LedgerReadPort>(
    port: &P,
    authorization: &TaxCommandAuthorization<'_>,
    request: LedgerCommandRequest,
) -> LedgerCommandOutput {
    if let Some(response) = tax_man_denial(authorization) {
        return LedgerCommandOutput {
            response,
            view: None,
        };
    }
    let limit = request.limit.clamp(1, LEDGER_MAX_LIMIT);
    let user_id = request.user.as_ref().map(|target| target.id);
    let Ok(total_entries) = port.count_ledger_entries(request.guild_id, user_id) else {
        return LedgerCommandOutput {
            response: CommandResponse::followup(
                "Couldn't load the central ledger right now.",
                true,
            ),
            view: None,
        };
    };
    let page = clamp_ledger_page(request.page.clamp(1, LEDGER_MAX_PAGE), limit, total_entries);
    let Ok(rows) =
        port.get_recent_ledger(request.guild_id, limit, ledger_offset(page, limit), user_id)
    else {
        return LedgerCommandOutput {
            response: CommandResponse::followup(
                "Couldn't load the central ledger right now.",
                true,
            ),
            view: None,
        };
    };
    let embed = build_ledger_embed(
        &rows,
        request.user.as_ref(),
        page,
        Some(total_entries),
        limit,
    );
    let view = (ledger_total_pages(total_entries, limit) > 1).then(|| {
        TaxLedgerView::new(
            request.guild_id,
            request.requester_id,
            request.user,
            limit,
            page,
            total_entries,
        )
    });
    LedgerCommandOutput {
        response: CommandResponse::embed(embed, true),
        view,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerCommandOutput {
    pub response: CommandResponse,
    pub view: Option<TaxPlayerLedgerView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerCommandRequest {
    pub guild_id: u64,
    pub requester_id: u64,
    pub user: UserRef,
}

#[must_use]
pub fn player_command<P: PlayerAuditReadPort>(
    port: &P,
    authorization: &TaxCommandAuthorization<'_>,
    request: PlayerCommandRequest,
) -> PlayerCommandOutput {
    if let Some(response) = tax_man_denial(authorization) {
        return PlayerCommandOutput {
            response,
            view: None,
        };
    }
    let Ok(snapshot) = port.get_player_snapshot(
        request.user.id,
        request.guild_id,
        PLAYER_LEDGER_DEFAULT_LIMIT,
        0,
    ) else {
        return PlayerCommandOutput {
            response: CommandResponse::followup(
                "Couldn't load that Tax Man player audit right now.",
                true,
            ),
            view: None,
        };
    };
    let Ok(total_entries) = port.count_ledger_entries(request.guild_id, Some(request.user.id))
    else {
        return PlayerCommandOutput {
            response: CommandResponse::followup(
                "Couldn't load that Tax Man player audit right now.",
                true,
            ),
            view: None,
        };
    };
    let embed = build_player_embed(
        &request.user,
        &snapshot,
        1,
        Some(total_entries),
        PLAYER_LEDGER_DEFAULT_LIMIT,
    );
    let view = (ledger_total_pages(total_entries, PLAYER_LEDGER_DEFAULT_LIMIT) > 1).then(|| {
        TaxPlayerLedgerView::new(
            request.guild_id,
            request.requester_id,
            request.user,
            PLAYER_LEDGER_DEFAULT_LIMIT,
            1,
            total_entries,
        )
    });
    PlayerCommandOutput {
        response: CommandResponse::embed(embed, true),
        view,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FineRequest {
    pub user_id: u64,
    pub guild_id: u64,
    pub amount: i64,
    pub actor_id: u64,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FineResult {
    Ok {
        requested_amount: i64,
        applied_amount: i64,
        balance_before: i64,
        balance_after: i64,
        next_fine_at: Option<i64>,
    },
    Cooldown {
        next_fine_at: i64,
    },
    NoOutstandingObligations,
    TargetNotRegistered,
    InvalidAmount,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResetFineCooldownRequest {
    pub user_id: u64,
    pub guild_id: u64,
    pub actor_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResetFineCooldownResult {
    Ok { had_cooldown: bool },
    TargetNotRegistered,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BankruptcyAction {
    Add { games: i64 },
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BankruptcyRequest {
    pub user_id: u64,
    pub guild_id: u64,
    pub actor_id: u64,
    pub action: BankruptcyAction,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BankruptcyResult {
    Added {
        games: i64,
        previous_games: i64,
        penalty_games_remaining: i64,
    },
    Removed {
        previous_games: i64,
        penalty_games_remaining: i64,
    },
    TargetNotRegistered,
    InvalidGames,
    InvalidAction,
    Rejected,
}

/// Transaction boundary for Tax Man mutations.
///
/// Implementations must update the player, reserve, cooldown/modifier state,
/// and central audit ledger in the same transaction. In particular, adapters
/// must not translate `levy_fine_atomic` into independent balance writes.
pub trait TaxEnforcementPort {
    type Error;

    fn levy_fine_atomic(&self, request: FineRequest) -> Result<FineResult, Self::Error>;

    fn reset_fine_cooldown_atomic(
        &self,
        request: ResetFineCooldownRequest,
    ) -> Result<ResetFineCooldownResult, Self::Error>;

    fn change_bankruptcy_modifier_atomic(
        &self,
        request: BankruptcyRequest,
    ) -> Result<BankruptcyResult, Self::Error>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FineCommandRequest {
    pub target: UserRef,
    pub guild_id: u64,
    pub actor_id: u64,
    pub amount: i64,
    pub reason: Option<String>,
}

#[must_use]
pub fn fine_command<P: TaxEnforcementPort>(
    port: &P,
    authorization: &TaxCommandAuthorization<'_>,
    command: FineCommandRequest,
) -> CommandResponse {
    if let Some(response) = tax_man_denial(authorization) {
        return response;
    }
    let request = FineRequest {
        user_id: command.target.id,
        guild_id: command.guild_id,
        amount: command.amount,
        actor_id: command.actor_id,
        reason: command.reason,
    };
    match port.levy_fine_atomic(request) {
        Ok(result) => CommandResponse::followup(format_fine_result(&command.target, &result), true),
        Err(_) => CommandResponse::followup("Couldn't levy that Tax Man fine right now.", true),
    }
}

#[must_use]
pub fn format_fine_result(user: &UserRef, result: &FineResult) -> String {
    match result {
        FineResult::Ok {
            requested_amount,
            applied_amount,
            balance_before,
            balance_after,
            next_fine_at,
        } => {
            let mut line = format!(
                "Levied a {} fine against {}. Balance: {} -> {}. Credited {} to the Jopacoin Reserve.",
                format_jc(*applied_amount),
                user.display_name,
                format_jc(*balance_before),
                format_jc(*balance_after),
                format_jc(*applied_amount),
            );
            if applied_amount < requested_amount {
                line.push_str(&format!(
                    " Requested {}, capped to audited obligations.",
                    format_jc(*requested_amount)
                ));
            }
            if let Some(next_fine_at) = next_fine_at {
                line.push_str(&format!(" Next fine available <t:{next_fine_at}:R>."));
            }
            line
        }
        FineResult::Cooldown { next_fine_at } => format!(
            "{} is still on Tax Man fine cooldown until <t:{next_fine_at}:f> (<t:{next_fine_at}:R>).",
            user.display_name
        ),
        FineResult::NoOutstandingObligations => format!(
            "{} has no audited outstanding obligations to fine.",
            user.display_name
        ),
        FineResult::TargetNotRegistered => "That user is not registered in this guild.".to_owned(),
        FineResult::InvalidAmount => "Fine amount must be positive.".to_owned(),
        FineResult::Rejected => "That fine could not be levied.".to_owned(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResetCooldownCommandRequest {
    pub target: UserRef,
    pub guild_id: u64,
    pub actor_id: u64,
}

#[must_use]
pub fn resetcooldown_command<P: TaxEnforcementPort>(
    port: &P,
    authorization: &TaxCommandAuthorization<'_>,
    command: ResetCooldownCommandRequest,
) -> CommandResponse {
    if let Some(response) = tax_man_denial(authorization) {
        return response;
    }
    let request = ResetFineCooldownRequest {
        user_id: command.target.id,
        guild_id: command.guild_id,
        actor_id: command.actor_id,
    };
    match port.reset_fine_cooldown_atomic(request) {
        Ok(result) => CommandResponse::followup(format_reset_result(&command.target, result), true),
        Err(_) => {
            CommandResponse::followup("Couldn't reset that Tax Man fine cooldown right now.", true)
        }
    }
}

fn format_reset_result(user: &UserRef, result: ResetFineCooldownResult) -> String {
    match result {
        ResetFineCooldownResult::Ok { had_cooldown: true } => {
            format!("Reset Tax Man fine cooldown for {}.", user.display_name)
        }
        ResetFineCooldownResult::Ok {
            had_cooldown: false,
        } => format!(
            "{} did not have an active Tax Man fine cooldown.",
            user.display_name
        ),
        ResetFineCooldownResult::TargetNotRegistered => {
            "That user is not registered in this guild.".to_owned()
        }
        ResetFineCooldownResult::Rejected => {
            "That Tax Man fine cooldown could not be reset.".to_owned()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BankruptcyCommandRequest {
    pub target: UserRef,
    pub guild_id: u64,
    pub actor_id: u64,
    pub action: BankruptcyAction,
    pub reason: Option<String>,
}

#[must_use]
pub fn bankruptcy_command<P: TaxEnforcementPort>(
    port: &P,
    authorization: &TaxCommandAuthorization<'_>,
    command: BankruptcyCommandRequest,
) -> CommandResponse {
    if let Some(response) = tax_man_denial(authorization) {
        return response;
    }
    let request = BankruptcyRequest {
        user_id: command.target.id,
        guild_id: command.guild_id,
        actor_id: command.actor_id,
        action: command.action,
        reason: command.reason,
    };
    match port.change_bankruptcy_modifier_atomic(request) {
        Ok(result) => {
            CommandResponse::followup(format_bankruptcy_result(&command.target, result), true)
        }
        Err(_) => {
            CommandResponse::followup("Couldn't update that bankruptcy modifier right now.", true)
        }
    }
}

fn format_bankruptcy_result(user: &UserRef, result: BankruptcyResult) -> String {
    match result {
        BankruptcyResult::Added {
            games,
            previous_games,
            penalty_games_remaining,
        } => format!(
            "Added {} bankruptcy modifier game(s) to {}. Penalty games: {} -> {}.",
            grouped(games),
            user.display_name,
            grouped(previous_games),
            grouped(penalty_games_remaining),
        ),
        BankruptcyResult::Removed {
            previous_games,
            penalty_games_remaining,
        } => format!(
            "Removed bankruptcy modifier from {}. Penalty games: {} -> {}.",
            user.display_name,
            grouped(previous_games),
            grouped(penalty_games_remaining),
        ),
        BankruptcyResult::TargetNotRegistered => {
            "That user is not registered in this guild.".to_owned()
        }
        BankruptcyResult::InvalidGames => {
            "Bankruptcy modifier games must be positive when adding.".to_owned()
        }
        BankruptcyResult::InvalidAction => "Unknown bankruptcy modifier action.".to_owned(),
        BankruptcyResult::Rejected => "That bankruptcy modifier could not be updated.".to_owned(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VanityEligibility {
    ManualTaxation,
    Taxable,
    NicknameExemption,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManualTaxationRequest {
    pub guild_id: u64,
    pub user_id: u64,
    pub enforced: bool,
    pub actor_id: u64,
}

/// The mutation must persist the override and return eligibility observed
/// after that same write. This prevents the clear response from racing a
/// separate cache/read operation.
pub trait VanityTaxPort {
    type Error;

    fn tax_rate_basis_points(&self) -> i64;
    fn tax_rate_fraction(&self) -> f64 {
        self.tax_rate_basis_points() as f64 / 10_000.0
    }
    fn eligibility_status(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<VanityEligibility, Self::Error>;
    fn set_manual_taxation_atomic(
        &self,
        request: ManualTaxationRequest,
    ) -> Result<VanityEligibility, Self::Error>;
}

#[must_use]
pub fn vanity_command<P: VanityTaxPort>(
    port: Option<&P>,
    permissions: &PermissionContext,
    admin_user_ids: &[u64],
    guild_id: u64,
    actor_id: u64,
    target: &UserRef,
    taxable: Option<bool>,
) -> CommandResponse {
    let Some(port) = port else {
        return CommandResponse::message("Vanity tax is not active.", true);
    };
    if let Some(enforced) = taxable {
        if !has_admin_permission(permissions, admin_user_ids) {
            return CommandResponse::message(
                "Only admins can change vanity-tax enforcement.",
                true,
            );
        }
        let request = ManualTaxationRequest {
            guild_id,
            user_id: target.id,
            enforced,
            actor_id,
        };
        return match port.set_manual_taxation_atomic(request) {
            Ok(_) if enforced => CommandResponse::followup(
                format!(
                    "**{}** is now manually subject to the vanity tax, regardless of their server nickname.",
                    target.display_name
                ),
                true,
            ),
            Ok(eligibility) => CommandResponse::followup(
                format!(
                    "**{}** returned to the automatic nickname rule and is {}.",
                    target.display_name,
                    automatic_vanity_status(eligibility)
                ),
                true,
            ),
            Err(_) => {
                CommandResponse::followup("Couldn't update vanity-tax enforcement right now.", true)
            }
        };
    }

    let Ok(eligibility) = port.eligibility_status(guild_id, target.id) else {
        return CommandResponse::message(
            "That vanity-tax status could not be loaded right now.",
            true,
        );
    };
    let rate_text = format_python_general(port.tax_rate_fraction() * 100.0);
    let message = match eligibility {
        VanityEligibility::ManualTaxation => format!(
            "**{}** is subject to the {rate_text}% vanity tax by an admin override, regardless of their server nickname.",
            target.display_name
        ),
        VanityEligibility::Taxable => format!(
            "🏷️ **{}** has no server nickname, so a **{rate_text}% vanity tax** is levied on profits — bets, digs, wheel wins, predictions, mafia, match bonuses.\nExemption: right-click your name → **Edit Server Profile** → set a nickname. The taxman notices immediately.",
            target.display_name
        ),
        VanityEligibility::NicknameExemption => format!(
            "✅ **{}** has a server nickname and is exempt from the {rate_text}% vanity tax. Remove it and the taxman returns.",
            target.display_name
        ),
        VanityEligibility::Unknown => format!(
            "**{}** is not currently in the server member cache, so their vanity-tax status cannot be determined.",
            target.display_name
        ),
    };
    CommandResponse::message(message, true)
}

/// Match Python's default `:g` rendering used by `/tax vanity`.
fn format_python_general(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value == f64::INFINITY {
        return "inf".to_owned();
    }
    if value == f64::NEG_INFINITY {
        return "-inf".to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }

    const SIGNIFICANT_DIGITS: usize = 6;
    let scientific = format!("{value:.5e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("Rust scientific float formatting includes an exponent");
    let exponent = exponent
        .parse::<i32>()
        .expect("Rust emits a numeric scientific exponent");
    if !(-4..SIGNIFICANT_DIGITS as i32).contains(&exponent) {
        return format!("{}e{exponent:+03}", trim_fractional_zeroes(mantissa));
    }
    let fractional_digits = (SIGNIFICANT_DIGITS as i32 - exponent - 1).max(0) as usize;
    trim_fractional_zeroes(&format!("{value:.fractional_digits$}"))
}

fn trim_fractional_zeroes(value: &str) -> String {
    if !value.contains('.') {
        return value.to_owned();
    }
    value.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn automatic_vanity_status(eligibility: VanityEligibility) -> &'static str {
    match eligibility {
        VanityEligibility::Taxable => "taxable because they have no server nickname",
        VanityEligibility::NicknameExemption => "exempt because they have a server nickname",
        VanityEligibility::ManualTaxation => "taxable by a newer admin override",
        VanityEligibility::Unknown => "not currently in the server member cache",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VanityMemberState {
    nickname_known: bool,
    has_nickname: bool,
    manually_taxed: bool,
}

#[derive(Debug)]
pub struct InMemoryVanityTax {
    rate_basis_points: i64,
    members: Mutex<BTreeMap<(u64, u64), VanityMemberState>>,
}

impl Default for InMemoryVanityTax {
    fn default() -> Self {
        Self::new(VANITY_TAX_BASIS_POINTS)
    }
}

impl InMemoryVanityTax {
    #[must_use]
    pub fn new(rate_basis_points: i64) -> Self {
        Self {
            rate_basis_points: rate_basis_points.clamp(0, 10_000),
            members: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn refresh_member(&self, guild_id: u64, user_id: u64, nickname: Option<&str>) {
        let mut members = self.members.lock().expect("vanity-tax mutex poisoned");
        let manual = members
            .get(&(guild_id, user_id))
            .is_some_and(|member| member.manually_taxed);
        members.insert(
            (guild_id, user_id),
            VanityMemberState {
                nickname_known: true,
                has_nickname: nickname.is_some(),
                manually_taxed: manual,
            },
        );
    }

    #[must_use]
    pub fn is_manually_taxed(&self, guild_id: u64, user_id: u64) -> bool {
        self.members
            .lock()
            .expect("vanity-tax mutex poisoned")
            .get(&(guild_id, user_id))
            .is_some_and(|member| member.manually_taxed)
    }

    #[must_use]
    pub fn calculate_tax(&self, user_id: u64, guild_id: u64, profit: i64) -> i64 {
        if profit <= 0 {
            return 0;
        }
        let eligibility = self
            .eligibility_status(guild_id, user_id)
            .expect("in-memory vanity eligibility is infallible");
        if matches!(
            eligibility,
            VanityEligibility::ManualTaxation | VanityEligibility::Taxable
        ) {
            profit.saturating_mul(self.rate_basis_points) / 10_000
        } else {
            0
        }
    }
}

impl VanityTaxPort for InMemoryVanityTax {
    type Error = std::convert::Infallible;

    fn tax_rate_basis_points(&self) -> i64 {
        self.rate_basis_points
    }

    fn eligibility_status(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<VanityEligibility, Self::Error> {
        let members = self.members.lock().expect("vanity-tax mutex poisoned");
        Ok(members
            .get(&(guild_id, user_id))
            .map_or(VanityEligibility::Unknown, |member| {
                if member.manually_taxed {
                    VanityEligibility::ManualTaxation
                } else if !member.nickname_known {
                    VanityEligibility::Unknown
                } else if member.has_nickname {
                    VanityEligibility::NicknameExemption
                } else {
                    VanityEligibility::Taxable
                }
            }))
    }

    fn set_manual_taxation_atomic(
        &self,
        request: ManualTaxationRequest,
    ) -> Result<VanityEligibility, Self::Error> {
        let mut members = self.members.lock().expect("vanity-tax mutex poisoned");
        let member = members
            .entry((request.guild_id, request.user_id))
            .or_insert(VanityMemberState {
                nickname_known: false,
                has_nickname: false,
                manually_taxed: false,
            });
        member.manually_taxed = request.enforced;
        Ok(if member.manually_taxed {
            VanityEligibility::ManualTaxation
        } else if !member.nickname_known {
            VanityEligibility::Unknown
        } else if member.has_nickname {
            VanityEligibility::NicknameExemption
        } else {
            VanityEligibility::Taxable
        })
    }
}

#[cfg(test)]
mod tests;
