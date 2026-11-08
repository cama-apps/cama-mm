//! `/gamba` wheel settlement: admission, wedge selection, outcome effects,
//! and the post-spin tail shared by direct and interactive spins.
//!
//! Every money movement goes through an existing atomic repository operation
//! keyed by the spin's event id, so a retried settlement never applies an
//! effect twice. The Discord provider only renders the typed results below.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use cama_db::bankruptcy_repository::BankruptcyRepository;
use cama_db::core_repositories::{MatchRepository, PlayerRepository, WheelSpinClaim};
use cama_db::dig_blood_pact::{DigBloodPactRepository, DigBloodPactSettlementRequest};
use cama_db::golden_wheel_repository::GoldenWheelRepository;
use cama_db::loan_repository::{LedgerContext, LoanRepository};
use cama_db::mana_service_repository::{BankruptBuff, ManaRepository};
use cama_db::match_recording_repository::{
    IncomeAwardReceipt, IncomeAwardRequest, MatchRecordingRepository,
};
use cama_db::pet_evolution_repository::PetEvolutionRepository;
use cama_db::shop_runtime::{
    HostileDestination as DbHostileDestination, HostileLossRequest as DbHostileLossRequest,
    HostileLossSettlement, ShopRuntimeRepository,
};
use cama_db::wheel_spin_repository::{NewWheelSpin, WheelSpinRepository};
use cama_domain::discord_content::UNKNOWN_PLAYER;
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::mana::ManaEffects;
use cama_domain::pet_evolution::PetActivity;
use cama_domain::player::Player;
use chrono::{DateTime, FixedOffset, Utc};
use rusqlite::types::Value as SqlValue;
use serde_json::json;

use crate::economy_actions::apply_gamba_event_multiplier;
use crate::golden_wheel::{LiveGoldenEconomy, compute_live_golden_wedges};
use crate::mana_service::mana_day_label;
use crate::wheel::{
    CooldownOutcome, LossRoll, WHEEL_BANANA_PEEL_LOSS_MAX, WHEEL_BANANA_PEEL_LOSS_MIN,
    WHEEL_BOMB_OMB_VICTIM_COUNT, WHEEL_BOMB_OMB_VICTIM_LOSS_MAX, WHEEL_BOMB_OMB_VICTIM_LOSS_MIN,
    WHEEL_GREEN_SHELL_STEAL_MAX, WHEEL_GREEN_SHELL_STEAL_MIN, WheelEconomyPolicy, WheelKind,
    WheelMechanic, WheelValue, WheelWedge, canonical_outcome_code, cooldown_after_resolution,
    get_wheel_wedges, next_wheel_reset_at, requested_percentage_or_flat, resolve_explosion,
    select_wheel_kind, wheel_window_start_at,
};

/// The deployment settings wheel settlement reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelSettlementConfig {
    pub garnishment_rate: f64,
    pub bankruptcy_penalty_rate_per_game: f64,
    pub vanity_tax_rate: f64,
    pub low_priority_tax_rate: f64,
    pub minigame_jc_delta_scale: f64,
    pub wheel_target_ev: f64,
    pub wheel_bankrupt_target_ev: f64,
    pub wheel_golden_target_ev: f64,
}

/// Readable names for the players a hostile outcome mentions.
pub trait WheelPlayerNameSource: Send + Sync {
    fn names_for(&self, player_ids: &[i64]) -> BTreeMap<i64, String>;
}

impl WheelPlayerNameSource for BTreeMap<i64, String> {
    fn names_for(&self, player_ids: &[i64]) -> BTreeMap<i64, String> {
        player_ids
            .iter()
            .filter_map(|player_id| self.get(player_id).map(|name| (*player_id, name.clone())))
            .collect()
    }
}

/// Schedules the wheel-ready DM once a spin's cooldown is recorded. Called
/// from a blocking worker.
pub trait WheelReminderPort: Send + Sync {
    fn schedule_wheel(&self, user_id: i64, guild_id: i64, ready_at: i64) -> Result<(), String>;
}

/// A deployment without reminder delivery.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoWheelReminders;

impl WheelReminderPort for NoWheelReminders {
    fn schedule_wheel(&self, _user_id: i64, _guild_id: i64, _ready_at: i64) -> Result<(), String> {
        Ok(())
    }
}

/// One selectable wedge of an interactive wheel.
#[derive(Clone, Debug)]
pub struct WheelOption {
    pub label: String,
    pub value: WheelValue,
    pub color: &'static str,
}

impl WheelOption {
    #[must_use]
    pub fn from_wedge(wedge: &WheelWedge) -> Self {
        Self {
            label: wedge.label.clone(),
            value: wedge.value,
            color: wedge.color,
        }
    }

    #[must_use]
    pub fn wedge(&self) -> WheelWedge {
        WheelWedge {
            label: self.label.clone(),
            value: self.value,
            color: self.color,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WheelInteractionKind {
    TownTrial,
    Discover,
    Scrying,
    Reroll,
}

/// What a pending wheel's first settlement attempt already fixed.
///
/// A failed settlement keeps the interaction for a later click or timeout
/// sweep, and both retries come from the provider's process-local map, so the
/// memo is exactly as durable as the retry it guards. Without it a retry
/// could pick a different option, re-claim a spent Red re-roll, or re-run
/// outcome effects (a consumed pardon, a penalty extension) the first attempt
/// committed.
#[derive(Clone, Debug, Default)]
pub struct WheelAttemptMemo {
    /// The option and re-roll choice the first attempt settled.
    selection: Option<(WheelOption, bool)>,
    /// The Red re-roll claim committed.
    reroll_claimed: bool,
    /// Seed for the outcome's random draws, so a retry re-draws them.
    rng_seed: Option<u64>,
    /// The committed outcome and the balance it was resolved against.
    outcome: Option<(i64, WheelResolution)>,
}

fn lock_wheel_attempt(attempt: &Mutex<WheelAttemptMemo>) -> MutexGuard<'_, WheelAttemptMemo> {
    attempt.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Fix the option a pending wheel settles with: the first attempt's choice
/// wins over a later click or timeout default.
pub fn pin_wheel_selection(
    attempt: &Mutex<WheelAttemptMemo>,
    option: WheelOption,
    use_reroll: bool,
) -> (WheelOption, bool) {
    lock_wheel_attempt(attempt)
        .selection
        .get_or_insert((option, use_reroll))
        .clone()
}

#[must_use]
pub fn wheel_cooldown_message(now: i64, last_spin: i64) -> String {
    let remaining = next_wheel_reset_at(last_spin).saturating_sub(now).max(0);
    let hours = remaining / 3_600;
    let minutes = (remaining % 3_600) / 60;
    format!("You already spun the wheel today! The wheel resets in **{hours}h {minutes}m**.")
}

/// Stable positive relation key for the ledger's integer exact-once marker.
/// Wheel event IDs intentionally retain human-readable components (`time`,
/// actor, random nonce), while the existing schema stores only an integer
/// relation. FNV-1a keeps retries and restart recovery deterministic.
pub fn event_related_id(event_id: &str) -> i64 {
    let hash = event_id
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3)
        });
    i64::try_from(hash & 0x7fff_ffff_ffff_ffff).unwrap_or(i64::MAX)
}

/// Mirror Python's best-effort pet activity side effect without coupling
/// betting settlement to pet evolution.  The activity repository owns the
/// existing-schema eligibility, daily cap, and exact-once source-key rules;
/// a missing/disabled pet therefore remains a quiet no-op.
pub fn record_betting_pet_activity(
    path: &Path,
    user_id: i64,
    guild_id: i64,
    activity: PetActivity,
    source_key: &str,
    occurred_at: i64,
) {
    let _ = PetEvolutionRepository::new(path).record_activity(
        user_id,
        Some(guild_id),
        activity,
        source_key,
        occurred_at,
    );
}

pub fn active_mana_effects(
    path: &Path,
    user_id: i64,
    guild_id: Option<i64>,
    now: i64,
) -> Result<ManaEffects, String> {
    let Some(row) = ManaRepository::new(path)
        .get_mana(user_id, guild_id)
        .map_err(|error| error.to_string())?
    else {
        return Ok(ManaEffects::default());
    };
    let offset = pacific_offset_seconds(now)?;
    let local = DateTime::<Utc>::from_timestamp(now, 0)
        .ok_or_else(|| "system timestamp is outside chrono's range".to_owned())?
        .with_timezone(
            &FixedOffset::east_opt(offset)
                .ok_or_else(|| "invalid Pacific time-zone offset".to_owned())?,
        );
    if row.assigned_date != mana_day_label(local) || row.consumed_today {
        return Ok(ManaEffects::default());
    }
    let color = match row.current_land.as_str() {
        "Island" => "Blue",
        "Mountain" => "Red",
        "Forest" => "Green",
        "Plains" => "White",
        "Swamp" => "Black",
        _ => return Ok(ManaEffects::default()),
    };
    Ok(ManaEffects::for_color(Some(color), Some(&row.current_land)))
}

/// Resolve the same Pacific 4AM day boundary used by the mana provider.
fn pacific_offset_seconds(timestamp: i64) -> Result<i32, String> {
    use chrono::{Datelike, NaiveDate, TimeZone, Weekday};

    let utc = DateTime::<Utc>::from_timestamp(timestamp, 0)
        .ok_or_else(|| "system timestamp is outside chrono's range".to_owned())?;
    let year = utc.year();
    let march =
        NaiveDate::from_ymd_opt(year, 3, 1).ok_or_else(|| "invalid March date".to_owned())?;
    let november =
        NaiveDate::from_ymd_opt(year, 11, 1).ok_or_else(|| "invalid November date".to_owned())?;
    let to_sunday = |date: NaiveDate| -> i64 {
        match date.weekday() {
            Weekday::Sun => 0,
            Weekday::Mon => 6,
            Weekday::Tue => 5,
            Weekday::Wed => 4,
            Weekday::Thu => 3,
            Weekday::Fri => 2,
            Weekday::Sat => 1,
        }
    };
    let second_sunday_march = march + chrono::Duration::days(to_sunday(march) + 7);
    let first_sunday_november = november + chrono::Duration::days(to_sunday(november));
    let standard = FixedOffset::west_opt(8 * 3_600)
        .ok_or_else(|| "invalid Pacific standard offset".to_owned())?;
    let daylight = FixedOffset::west_opt(7 * 3_600)
        .ok_or_else(|| "invalid Pacific daylight offset".to_owned())?;
    let starts = standard
        .from_local_datetime(
            &second_sunday_march
                .and_hms_opt(2, 0, 0)
                .ok_or_else(|| "invalid DST start time".to_owned())?,
        )
        .single()
        .ok_or_else(|| "ambiguous DST start".to_owned())?
        .timestamp();
    let ends = daylight
        .from_local_datetime(
            &first_sunday_november
                .and_hms_opt(2, 0, 0)
                .ok_or_else(|| "invalid DST end time".to_owned())?,
        )
        .single()
        .ok_or_else(|| "ambiguous DST end".to_owned())?
        .timestamp();
    Ok(if (starts..ends).contains(&timestamp) {
        -7 * 3_600
    } else {
        -8 * 3_600
    })
}

fn apply_mana_wheel_modifiers(
    wedges: &mut [crate::wheel::WheelWedge],
    effects: &ManaEffects,
    kind: WheelKind,
) {
    // Python applies wheel mana variance/caps only to the ordinary wheel.
    // Bankrupt and Golden catalogs retain their authored negative/hostile
    // mechanics; their live calibration is handled by the shared policy.
    if kind != WheelKind::Regular {
        return;
    }
    if effects.color.as_deref() == Some("Green") {
        for wedge in wedges.iter_mut() {
            if let WheelValue::Numeric(value) = wedge.value {
                let adjusted = value
                    .max(effects.green_bankrupt_penalty)
                    .min(effects.green_max_wheel_win);
                wedge.value = WheelValue::Numeric(adjusted);
                wedge.label = adjusted.to_string();
            }
        }
    }
    if effects.color.as_deref() == Some("White")
        && let Some(cap) = effects.plains_max_wheel_win
    {
        for wedge in wedges.iter_mut() {
            if let WheelValue::Numeric(value) = wedge.value
                && value > cap
            {
                wedge.value = WheelValue::Numeric(cap);
                wedge.label = cap.to_string();
            }
        }
    }
    {
        let replacement = match effects.color.as_deref() {
            Some("Red") => Some(("ERUPTION", WheelMechanic::Eruption, "#ff4500")),
            Some("Green") => Some(("GROWTH", WheelMechanic::Overgrowth, "#228b22")),
            Some("Black") => Some(("DECAY", WheelMechanic::Decay, "#4b0082")),
            _ => None,
        };
        if let Some((label, mechanic, color)) = replacement
            && let Some(wedge) = wedges.iter_mut().find(|wedge| {
                matches!(wedge.value, WheelValue::Numeric(value) if (15..=25).contains(&value))
            })
        {
            wedge.label = label.to_owned();
            wedge.value = WheelValue::Mechanic(mechanic);
            wedge.color = color;
        }
    }
}

fn wheel_value_json(value: WheelValue) -> serde_json::Value {
    match value {
        WheelValue::Numeric(value) => json!(value),
        WheelValue::Mechanic(mechanic) => json!(mechanic.code()),
    }
}

fn wheel_metadata_json(value: serde_json::Value) -> cama_db::wheel_spin_repository::JsonValue {
    match value {
        serde_json::Value::Null => cama_db::wheel_spin_repository::JsonValue::Null,
        serde_json::Value::Bool(value) => cama_db::wheel_spin_repository::JsonValue::Bool(value),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(cama_db::wheel_spin_repository::JsonValue::Integer)
            .or_else(|| {
                value
                    .as_f64()
                    .map(cama_db::wheel_spin_repository::JsonValue::Float)
            })
            .unwrap_or(cama_db::wheel_spin_repository::JsonValue::Null),
        serde_json::Value::String(value) => {
            cama_db::wheel_spin_repository::JsonValue::String(value)
        }
        serde_json::Value::Array(values) => cama_db::wheel_spin_repository::JsonValue::Array(
            values.into_iter().map(wheel_metadata_json).collect(),
        ),
        serde_json::Value::Object(values) => cama_db::wheel_spin_repository::JsonValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, wheel_metadata_json(value)))
                .collect(),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn log_wheel_spin(
    path: &Path,
    user_id: i64,
    guild_id: i64,
    result: i64,
    now: i64,
    kind: WheelKind,
    is_bonus: bool,
    outcome_code: &str,
    event_id: &str,
    metadata: serde_json::Value,
) -> Result<(), String> {
    WheelSpinRepository::new(path)
        .log_wheel_spin(&NewWheelSpin {
            discord_id: user_id,
            guild_id: Some(guild_id),
            result,
            spin_time: now,
            is_bankrupt: kind == WheelKind::Bankrupt,
            is_golden: kind == WheelKind::Golden,
            outcome_code: Some(outcome_code.to_owned()),
            is_bonus,
            event_id: Some(event_id.to_owned()),
            outcome_metadata: Some(wheel_metadata_json(metadata)),
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
pub fn credit_wheel_income(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    _balance_before: i64,
    gross: i64,
    config: &WheelSettlementConfig,
    event_id: &str,
    reason: &str,
    vanity_taxable_ids: &BTreeSet<i64>,
    low_priority_taxable_ids: &BTreeSet<i64>,
) -> Result<IncomeAwardReceipt, String> {
    if gross <= 0 {
        let balance = PlayerRepository::new(path)
            .get_by_id(user_id, Some(guild_id))
            .map_err(|error| error.to_string())?
            .map(|player| player.jopacoin_balance)
            .unwrap_or_default();
        return Ok(IncomeAwardReceipt {
            discord_id: user_id,
            gross,
            balance_after: balance,
            net: gross,
            ..IncomeAwardReceipt::default()
        });
    }
    MatchRecordingRepository::new(path)
        .credit_income_awards_atomic(
            Some(guild_id),
            &[IncomeAwardRequest {
                discord_id: user_id,
                gross,
                garnishment_rate: config.garnishment_rate,
                apply_bankruptcy_penalty: true,
                bankruptcy_penalty_rate_per_game: config.bankruptcy_penalty_rate_per_game,
                vanity_tax_rate: if vanity_taxable_ids.contains(&user_id) {
                    config.vanity_tax_rate
                } else {
                    0.0
                },
                low_priority_tax_rate: if low_priority_taxable_ids.contains(&user_id) {
                    config.low_priority_tax_rate
                } else {
                    0.0
                },
                decrement_bankruptcy_penalty: false,
                source: "gamba",
                related_type: Some("wheel_spin"),
                related_id: Some(event_related_id(event_id)),
                reason,
                exact_once: true,
            }],
        )
        .map_err(|error| error.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "wheel income credit returned no receipt".to_owned())
}

/// Apply the post-outcome Blood Pact step from Python's wheel processor.
///
/// The wheel can settle several independent transfers before it reaches this
/// boundary, so `earning` is the net the outcome's own receipts moved into the
/// spinner's balance rather than the landed wedge value. A live balance
/// difference would also skim tips or bet payouts that landed meanwhile. The
/// hostile-loss event key makes a retry after a process stop return the
/// already-settled transfer without consuming another portion of the pact cap.
pub fn apply_wheel_blood_pact_skim(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    earning: i64,
    event_id: &str,
    now: i64,
    minigame_scale: f64,
) -> Result<(i64, i64), String> {
    let players = PlayerRepository::new(path);
    let current = players
        .get_by_id(user_id, Some(guild_id))
        .map_err(|error| error.to_string())?
        .map(|player| player.jopacoin_balance)
        .unwrap_or_default();
    if earning <= 0 {
        return Ok((0, current));
    }
    let event_key = format!("gamba-blood-pact:{event_id}:{user_id}");
    let shop = ShopRuntimeRepository::new(path);
    if let Some(existing) = shop
        .hostile_loss_event(guild_id, user_id, &event_key)
        .map_err(|error| error.to_string())?
    {
        return Ok((existing.applied, current));
    }
    let mana_date = {
        let offset = pacific_offset_seconds(now)?;
        let local = DateTime::<Utc>::from_timestamp(now, 0)
            .ok_or_else(|| "system timestamp is outside chrono's range".to_owned())?
            .with_timezone(
                &FixedOffset::east_opt(offset)
                    .ok_or_else(|| "invalid Pacific time-zone offset".to_owned())?,
            );
        mana_day_label(local)
    };
    // Reserve, protect, transfer, and account the skim in one transaction.
    let skimmed = DigBloodPactRepository::new(path)
        .settle(&DigBloodPactSettlementRequest {
            target_id: user_id,
            guild_id,
            earning,
            event_key,
            minigame_jc_delta_scale: minigame_scale,
            occurred_at: now,
            mana_date,
        })
        .map_err(|error| error.to_string())?
        .applied_amount;
    let balance_after = players
        .get_by_id(user_id, Some(guild_id))
        .map_err(|error| error.to_string())?
        .map(|player| player.jopacoin_balance)
        .unwrap_or(current);
    Ok((skimmed, balance_after))
}

#[allow(clippy::too_many_arguments)]
pub fn apply_hostile_wheel_loss(
    path: &Path,
    guild_id: i64,
    victim_id: i64,
    actor_id: i64,
    requested: i64,
    kind: &str,
    event_id: &str,
    destination: DbHostileDestination,
    recipient_id: Option<i64>,
    min_balance: Option<i64>,
    clamp_to_balance: bool,
    protect_self: bool,
    now: i64,
) -> Result<cama_db::shop_runtime::HostileLossSettlement, String> {
    if requested <= 0 {
        return Err("wheel hostile amount must be positive".to_owned());
    }
    let request = DbHostileLossRequest {
        victim_id,
        guild_id,
        requested,
        kind: kind.to_owned(),
        actor_id: Some(actor_id),
        event_key: event_id.to_owned(),
        destination,
        recipient_id,
        clamp_to_balance,
        min_balance,
        protect_self,
        metadata: json!({"outcome": kind}),
        occurred_at: now,
        mana_date: wheel_mana_date(now)?,
    };
    ShopRuntimeRepository::new(path)
        .apply_hostile_losses_once(std::slice::from_ref(&request))
        .map_err(|error| error.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "wheel hostile loss returned no settlement".to_owned())?
}

fn wheel_mana_date(now: i64) -> Result<String, String> {
    let offset = pacific_offset_seconds(now)?;
    let local = DateTime::<Utc>::from_timestamp(now, 0)
        .ok_or_else(|| "system timestamp is outside chrono's range".to_owned())?
        .with_timezone(
            &FixedOffset::east_opt(offset)
                .ok_or_else(|| "invalid Pacific time-zone offset".to_owned())?,
        );
    Ok(mana_day_label(local))
}

/// One victim a wheel outcome charges.
#[derive(Clone, Copy, Debug)]
struct PlannedWheelLoss {
    victim_id: i64,
    requested: i64,
    destination: DbHostileDestination,
    recipient_id: Option<i64>,
    clamp: bool,
}

/// Settle every victim of one wheel outcome in a single transaction, keyed by
/// `{event_id}:{victim_id}`.
///
/// Either the whole outcome commits or none of it does, and a retried
/// settlement reuses each recorded loss instead of re-deriving it from
/// balances the first attempt already moved. Non-positive amounts are not
/// charged, and a victim the repository rejects is skipped with a warning
/// rather than failing the spin.
#[allow(clippy::too_many_arguments)]
fn settle_wheel_losses(
    path: &Path,
    guild_id: i64,
    actor_id: i64,
    kind: &str,
    event_id: &str,
    plan: &[PlannedWheelLoss],
    min_balance: Option<i64>,
    now: i64,
) -> Result<SettledWheelLosses, String> {
    let plan = plan
        .iter()
        .filter(|planned| planned.requested > 0)
        .copied()
        .collect::<Vec<_>>();
    if plan.is_empty() {
        return Ok(SettledWheelLosses::default());
    }
    let mana_date = wheel_mana_date(now)?;
    let requests = plan
        .iter()
        .map(|planned| DbHostileLossRequest {
            victim_id: planned.victim_id,
            guild_id,
            requested: planned.requested,
            kind: kind.to_owned(),
            actor_id: Some(actor_id),
            event_key: format!("{event_id}:{}", planned.victim_id),
            destination: planned.destination,
            recipient_id: planned.recipient_id,
            clamp_to_balance: planned.clamp,
            min_balance,
            protect_self: false,
            metadata: json!({"outcome": kind}),
            occurred_at: now,
            mana_date: mana_date.clone(),
        })
        .collect::<Vec<_>>();
    let results = ShopRuntimeRepository::new(path)
        .apply_hostile_losses_once(&requests)
        .map_err(|error| error.to_string())?;
    let mut settled = SettledWheelLosses::default();
    for (planned, result) in plan.into_iter().zip(results) {
        match result {
            Ok(settlement) => settled.settled.push((planned, settlement)),
            Err(error) => settled.warnings.push(format!(
                "skipped invalid {kind} victim {}: {error}",
                planned.victim_id
            )),
        }
    }
    Ok(settled)
}

fn overgrowth_reward(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    effects: &ManaEffects,
    minigame_scale: f64,
) -> Result<i64, String> {
    let since = Utc::now() - chrono::Duration::days(7);
    let matches = match MatchRepository::new(path).get_player_matches(user_id, Some(guild_id), 100)
    {
        Ok(matches) => matches,
        Err(_) => {
            return Ok(cama_domain::economy_scaling::scale_minigame_jc_delta(
                10.0,
                minigame_scale,
            ));
        }
    };
    let games_this_week = matches
        .iter()
        .filter(|player_match| match &player_match.match_date {
            SqlValue::Text(value) => {
                parse_match_timestamp(value.as_str()).is_some_and(|timestamp| timestamp >= since)
            }
            _ => false,
        })
        .count() as i64;
    let base = games_this_week.saturating_mul(10).max(10);
    let capped = effects
        .green_gain_cap
        .map_or(base, |cap| base.min(cap.max(0)));
    Ok(cama_domain::economy_scaling::scale_minigame_jc_delta(
        capped as f64,
        minigame_scale,
    ))
}

/// Apply the daily gamba event exactly once to a newly minted wheel reward.
///
/// Numeric wedges are adjusted at admission, while mechanic rewards reach
/// this helper after their central minigame scaling. Keeping the two stages in
/// one small policy function prevents Eruption/Chain/Dividend and hostile
/// fallback credits from accidentally applying the event twice.
fn scale_minted_gamba_reward(
    base: i64,
    event_win_multiplier: f64,
    event_loss_multiplier: f64,
    bonus_spin: bool,
) -> i64 {
    let adjusted = apply_gamba_event_multiplier(base, event_win_multiplier, event_loss_multiplier);
    if bonus_spin {
        crate::wheel::scale_positive_dig_jc(adjusted)
    } else {
        adjusted
    }
}

fn parse_match_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                .map(|timestamp| DateTime::<Utc>::from_naive_utc_and_offset(timestamp, Utc))
        })
        .ok()
}

/// What one landed wedge did, as later steps and presentation need it.
#[derive(Clone, Debug)]
pub struct WheelResolution {
    pub resolved: WheelValue,
    /// Numeric result persisted to wheel history.
    pub logged: i64,
    pub cooldown_outcome: CooldownOutcome,
    pub direct_loss_settled: bool,
    pub extra_note: String,
    pub income: Option<IncomeAwardReceipt>,
    /// Lightning is a server-wide outcome whose Neon hook needs both the
    /// aggregate tax and the number of accounts actually charged.  Keeping
    /// that side effect beside the policy result avoids reconstructing it
    /// from a later, potentially changed leaderboard.
    pub neon_lightning: Option<(i64, usize)>,
    /// Net JopaCoin the outcome's own receipts moved into the spinner's
    /// balance. Blood Pact skims this, never a live balance difference that
    /// would also include unrelated credits landing meanwhile.
    pub earning: i64,
    /// Victims the hostile-loss repository rejected and the outcome skipped.
    pub warnings: Vec<String>,
}

impl WheelResolution {
    fn new(
        resolved: WheelValue,
        logged: i64,
        cooldown_outcome: CooldownOutcome,
        extra_note: String,
    ) -> Self {
        Self {
            resolved,
            logged,
            cooldown_outcome,
            direct_loss_settled: false,
            extra_note,
            income: None,
            neon_lightning: None,
            earning: 0,
            warnings: Vec::new(),
        }
    }

    fn with_warnings(mut self, warnings: Vec<String>) -> Self {
        self.warnings = warnings;
        self
    }

    const fn with_earning(mut self, earning: i64) -> Self {
        self.earning = earning;
        self
    }

    fn with_lightning(mut self, total: i64, players: usize) -> Self {
        self.neon_lightning = Some((total, players));
        self
    }

    fn with_direct_loss_settled(mut self) -> Self {
        self.direct_loss_settled = true;
        self
    }

    fn with_income(mut self, income: IncomeAwardReceipt) -> Self {
        self.earning = income.balance_delta;
        self.income = Some(income);
        self
    }
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_wheel_value(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    balance_before: i64,
    landed: &crate::wheel::WheelWedge,
    kind: WheelKind,
    effects: &ManaEffects,
    config: &WheelSettlementConfig,
    event_id: &str,
    vanity_taxable_ids: &BTreeSet<i64>,
    low_priority_taxable_ids: &BTreeSet<i64>,
    event_win_multiplier: f64,
    event_loss_multiplier: f64,
    bonus_spin: bool,
    visible_positive_balance: Option<i64>,
    visible_member_ids: Option<&BTreeSet<i64>>,
    player_names: &dyn WheelPlayerNameSource,
    rng_seed: u64,
    now: i64,
) -> Result<WheelResolution, String> {
    let mechanic = match landed.value {
        WheelValue::Numeric(value) => {
            // Both direct and interactive selections arrive here before the
            // daily-event adjustment. Applying it at this single boundary
            // avoids double-scaling numeric wedges while letting minted
            // mechanic rewards use the same event snapshot.
            let adjusted =
                apply_gamba_event_multiplier(value, event_win_multiplier, event_loss_multiplier);
            if adjusted > 0 {
                let mut adjusted = adjusted;
                if bonus_spin {
                    adjusted = crate::wheel::scale_positive_dig_jc(adjusted);
                }
                if effects.blue_gamba_reduction > 0.0 {
                    adjusted = adjusted
                        .saturating_sub((adjusted as f64 * effects.blue_gamba_reduction) as i64);
                }
                // Python parity: `_numeric_result` applies only dig scaling
                // and the Blue reduction. The Green gain cap belongs solely
                // to the Overgrowth mechanic (`wheel_outcomes.py:510`), so
                // capping every positive wedge here shorted Green players.
                let receipt = credit_wheel_income(
                    path,
                    guild_id,
                    user_id,
                    balance_before,
                    adjusted,
                    config,
                    event_id,
                    "gamba wheel payout",
                    vanity_taxable_ids,
                    low_priority_taxable_ids,
                )?;
                return Ok(WheelResolution::new(
                    WheelValue::Numeric(adjusted),
                    adjusted,
                    CooldownOutcome::Other,
                    wheel_income_note(&receipt),
                )
                .with_income(receipt));
            }
            if adjusted < 0
                && kind == WheelKind::Bankrupt
                && PlayerRepository::new(path)
                    .consume_wheel_pardon_atomic(user_id, Some(guild_id))
                    .map_err(|error| error.to_string())?
            {
                // Python parity: the pardon rewrites the result to 0, which
                // `commands/betting.py` treats as Lose-a-Turn — the pardoned
                // spinner still serves the extended penalty cooldown.
                return Ok(WheelResolution::new(
                    WheelValue::Numeric(0),
                    0,
                    CooldownOutcome::LoseTurn,
                    "🛡️ Comeback pardon consumed: the bankrupt loss was forgiven.".to_owned(),
                ));
            }
            if adjusted < 0 {
                let settlement = apply_hostile_wheel_loss(
                    path,
                    guild_id,
                    user_id,
                    user_id,
                    adjusted.saturating_abs(),
                    "wheel_loss",
                    event_id,
                    DbHostileDestination::Reserve,
                    None,
                    None,
                    false,
                    true,
                    now,
                )?;
                let applied = settlement.applied;
                let shield_note = if settlement.absorbed > 0 {
                    format!(
                        "🌾 White Mana Shields: **{}** shield activation(s) absorbed **{}** {}.",
                        1, settlement.absorbed, JOPACOIN_EMOTE,
                    )
                } else {
                    String::new()
                };
                return Ok(WheelResolution::new(
                    WheelValue::Numeric(-applied),
                    -applied,
                    CooldownOutcome::Other,
                    shield_note,
                )
                .with_direct_loss_settled()
                .with_earning(-applied));
            }
            return Ok(WheelResolution::new(
                WheelValue::Numeric(adjusted),
                adjusted,
                if adjusted == 0 {
                    CooldownOutcome::LoseTurn
                } else {
                    CooldownOutcome::Other
                },
                String::new(),
            ));
        }
        WheelValue::Mechanic(mechanic) => mechanic,
    };
    let minted_reward = |base: i64| {
        scale_minted_gamba_reward(
            base,
            event_win_multiplier,
            event_loss_multiplier,
            bonus_spin,
        )
    };
    match mechanic {
        WheelMechanic::ExtendOne | WheelMechanic::ExtendTwo => {
            let delta = if mechanic == WheelMechanic::ExtendOne {
                1
            } else {
                2
            };
            let bankruptcy = BankruptcyRepository::new(path);
            // Python only extends an existing bankruptcy penalty. A player
            // can still land an EXTEND wedge after recovering from debt; in
            // that case the result is deliberately a no-op rather than a new
            // penalty state.
            if bankruptcy
                .get_penalty_games(user_id, Some(guild_id))
                .map_err(|error| error.to_string())?
                > 0
            {
                bankruptcy
                    .adjust_penalty_games_atomic(user_id, Some(guild_id), delta)
                    .map_err(|error| error.to_string())?;
            }
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                0,
                CooldownOutcome::Other,
                String::new(),
            ))
        }
        WheelMechanic::Jailbreak => {
            BankruptcyRepository::new(path)
                .adjust_penalty_games_atomic(user_id, Some(guild_id), -1)
                .map_err(|error| error.to_string())?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                0,
                CooldownOutcome::Other,
                String::new(),
            ))
        }
        WheelMechanic::Eruption => {
            let last_spin = WheelSpinRepository::new(path)
                .get_last_normal_wheel_spin(Some(guild_id))
                .map_err(|error| error.to_string())?;
            let base_reward = last_spin
                .map(|spin| spin.result.saturating_abs().saturating_mul(2))
                .filter(|reward| *reward > 0)
                .unwrap_or_else(|| {
                    cama_domain::economy_scaling::scale_minigame_jc_delta(
                        50.0,
                        config.minigame_jc_delta_scale,
                    )
                });
            let reward = minted_reward(base_reward);
            let receipt = credit_wheel_income(
                path,
                guild_id,
                user_id,
                balance_before,
                reward,
                config,
                event_id,
                "gamba eruption fallback credit",
                vanity_taxable_ids,
                low_priority_taxable_ids,
            )?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                0,
                CooldownOutcome::Other,
                wheel_income_note(&receipt),
            )
            .with_income(receipt))
        }
        WheelMechanic::Overgrowth => {
            let reward = minted_reward(overgrowth_reward(
                path,
                guild_id,
                user_id,
                effects,
                config.minigame_jc_delta_scale,
            )?);
            let receipt = credit_wheel_income(
                path,
                guild_id,
                user_id,
                balance_before,
                reward,
                config,
                event_id,
                "gamba overgrowth reward",
                vanity_taxable_ids,
                low_priority_taxable_ids,
            )?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                0,
                CooldownOutcome::Other,
                wheel_income_note(&receipt),
            )
            .with_income(receipt))
        }
        WheelMechanic::CompoundInterest => {
            let reward = minted_reward(100);
            let receipt = credit_wheel_income(
                path,
                guild_id,
                user_id,
                balance_before,
                reward,
                config,
                event_id,
                "gamba compound interest credit",
                vanity_taxable_ids,
                low_priority_taxable_ids,
            )?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                reward,
                CooldownOutcome::Other,
                wheel_income_note(&receipt),
            )
            .with_income(receipt))
        }
        WheelMechanic::Dividend => {
            let total = visible_positive_balance.unwrap_or(
                GoldenWheelRepository::new(path)
                    .get_total_positive_balance(Some(guild_id))
                    .map_err(|error| error.to_string())?,
            );
            let reward = minted_reward(((total as f64 * 0.005) as i64).max(10));
            let receipt = credit_wheel_income(
                path,
                guild_id,
                user_id,
                balance_before,
                reward,
                config,
                event_id,
                "gamba dividend credit",
                vanity_taxable_ids,
                low_priority_taxable_ids,
            )?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                reward,
                CooldownOutcome::Other,
                wheel_income_note(&receipt),
            )
            .with_income(receipt))
        }
        WheelMechanic::Recession => {
            let recession = resolve_recession(
                path,
                guild_id,
                user_id,
                event_id,
                config.minigame_jc_delta_scale,
                visible_member_ids,
                now,
            )?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                -recession.self_loss,
                CooldownOutcome::Other,
                format!(
                    "**{}** player(s) lost a slice of their balance. You lost **{}** {}. Total drained from the server: **{}** {} (sent to the Jopacoin Reserve).",
                    recession.count,
                    recession.self_loss,
                    JOPACOIN_EMOTE,
                    recession.total,
                    JOPACOIN_EMOTE,
                ),
            )
            .with_earning(-recession.self_loss)
            .with_warnings(recession.warnings))
        }
        WheelMechanic::RedShell
        | WheelMechanic::BlueShell
        | WheelMechanic::BananaPeel
        | WheelMechanic::GreenShell
        | WheelMechanic::BombOmb
        | WheelMechanic::LightningBolt
        | WheelMechanic::Commune
        | WheelMechanic::TrickleDown
        | WheelMechanic::Heist
        | WheelMechanic::MarketCrash
        | WheelMechanic::HostileTakeover
        | WheelMechanic::Emergency
        | WheelMechanic::Decay => {
            let hostile = resolve_hostile_mechanic(
                path,
                guild_id,
                user_id,
                balance_before,
                mechanic,
                event_id,
                config,
                vanity_taxable_ids,
                low_priority_taxable_ids,
                event_win_multiplier,
                visible_member_ids,
                rng_seed,
                now,
            )?;
            let player_ids = hostile
                .victim_id
                .into_iter()
                .chain(hostile.victims.iter().map(|(player_id, _)| *player_id))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let rendered_names = player_names.names_for(&player_ids);
            let resolution = WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                hostile.log_result,
                CooldownOutcome::Other,
                hostile_result_note(mechanic, &hostile, config, &rendered_names),
            )
            .with_earning(hostile.spinner_delta)
            .with_warnings(hostile.warnings.clone());
            if hostile.lightning_total > 0 {
                Ok(resolution.with_lightning(hostile.lightning_total, hostile.lightning_count))
            } else {
                Ok(resolution)
            }
        }
        WheelMechanic::Comeback => {
            PlayerRepository::new(path)
                .set_wheel_pardon(user_id, Some(guild_id), true)
                .map_err(|error| error.to_string())?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                0,
                CooldownOutcome::Other,
                "🛡️ Comeback pardon armed: your next bankrupt loss is forgiven.".to_owned(),
            ))
        }
        WheelMechanic::ChainReaction => {
            let previous = WheelSpinRepository::new(path)
                .get_last_normal_wheel_spin(Some(guild_id))
                .map_err(|error| error.to_string())?;
            let Some(previous) = previous else {
                return Ok(WheelResolution::new(
                    WheelValue::Mechanic(mechanic),
                    0,
                    CooldownOutcome::Other,
                    "No previous ordinary spin is available to copy.".to_owned(),
                ));
            };
            let copied_note = || {
                let names = player_names.names_for(&[previous.discord_id]);
                format!(
                    "Copied {}'s previous **{}** {} outcome.",
                    names
                        .get(&previous.discord_id)
                        .map_or(UNKNOWN_PLAYER, String::as_str),
                    previous.result,
                    JOPACOIN_EMOTE
                )
            };
            if previous.result > 0 {
                let reward = minted_reward(previous.result);
                let receipt = credit_wheel_income(
                    path,
                    guild_id,
                    user_id,
                    balance_before,
                    reward,
                    config,
                    event_id,
                    "gamba chain reaction credit",
                    vanity_taxable_ids,
                    low_priority_taxable_ids,
                )?;
                Ok(WheelResolution::new(
                    WheelValue::Mechanic(mechanic),
                    0,
                    CooldownOutcome::Other,
                    copied_note(),
                )
                .with_income(receipt))
            } else if previous.result < 0 {
                // The spin's event id is the transfer's exact-once identity,
                // so a retried settlement cannot debit the copied loss twice.
                let transfer = LoanRepository::new(path)
                    .transfer_balance_to_nonprofit_once_atomic(
                        user_id,
                        Some(guild_id),
                        previous.result.saturating_abs(),
                        &LedgerContext {
                            source: Some("gamba".to_owned()),
                            actor_id: Some(user_id),
                            related_type: Some("wheel_spin".to_owned()),
                            related_id: Some(event_id.to_owned()),
                            reason: Some("gamba chain reaction loss reserve credit".to_owned()),
                            metadata: Some(
                                json!({"copied_spin_user_id": previous.discord_id}).to_string(),
                            ),
                        },
                    )
                    .map_err(|error| error.to_string())?;
                Ok(WheelResolution::new(
                    WheelValue::Mechanic(mechanic),
                    0,
                    CooldownOutcome::Other,
                    copied_note(),
                )
                .with_earning(-transfer.amount))
            } else {
                Ok(WheelResolution::new(
                    WheelValue::Mechanic(mechanic),
                    0,
                    CooldownOutcome::Other,
                    "The previous spin had no numeric outcome to copy.".to_owned(),
                ))
            }
        }
        WheelMechanic::TownTrial | WheelMechanic::Discover => Ok(WheelResolution::new(
            WheelValue::Mechanic(mechanic),
            0,
            CooldownOutcome::Other,
            "Interactive wheel outcome requires an active Discord view.".to_owned(),
        )),
    }
}

fn wheel_income_note(receipt: &IncomeAwardReceipt) -> String {
    let mut notes = Vec::new();
    if receipt.garnished > 0 {
        notes.push(format!(
            "{} {} garnished",
            receipt.garnished, JOPACOIN_EMOTE
        ));
    }
    if receipt.bankruptcy_penalty > 0 {
        notes.push(format!(
            "{} {} bankruptcy penalty",
            receipt.bankruptcy_penalty, JOPACOIN_EMOTE
        ));
    }
    if receipt.vanity_tax > 0 {
        notes.push(format!(
            "{} {} vanity tax",
            receipt.vanity_tax, JOPACOIN_EMOTE
        ));
    }
    if receipt.low_priority_tax > 0 {
        notes.push(format!(
            "{} {} low priority tax",
            receipt.low_priority_tax, JOPACOIN_EMOTE
        ));
    }
    if notes.is_empty() {
        String::new()
    } else {
        format!("\n{}", notes.join(" | "))
    }
}

#[derive(Clone, Debug, Default)]
struct RecessionResolution {
    total: i64,
    count: usize,
    self_loss: i64,
    warnings: Vec<String>,
}

fn resolve_recession(
    path: &Path,
    guild_id: i64,
    spinner_id: i64,
    event_id: &str,
    scale: f64,
    visible_member_ids: Option<&BTreeSet<i64>>,
    now: i64,
) -> Result<RecessionResolution, String> {
    // Python parity: every rank-based wheel mechanic reads the guild-member
    // visibility-filtered leaderboard (`wheel_outcomes.py::_leaderboard`), so
    // departed ex-members neither hold ranks nor pay the tax.
    let leaderboard = filter_visible_wheel_leaderboard(
        PlayerRepository::new(path)
            .get_leaderboard(Some(guild_id), i64::from(i32::MAX), 0)
            .map_err(|error| error.to_string())?,
        visible_member_ids,
    );
    let mut plan = Vec::new();
    for (rank, player) in leaderboard.iter().enumerate() {
        if player.jopacoin_balance < crate::wheel::HOSTILE_LOSS_MIN_BALANCE {
            continue;
        }
        let (percentage, minimum) = if rank < 3 {
            (0.06, 50_i64)
        } else if rank < 10 {
            (0.035, 10_i64)
        } else {
            (0.02, 1_i64)
        };
        let requested = cama_domain::economy_scaling::scale_minigame_jc_delta(
            (minimum.max((player.jopacoin_balance as f64 * percentage) as i64)) as f64,
            scale,
        )
        .min(player.jopacoin_balance)
        .max(0);
        let Some(victim_id) = player.discord_id else {
            continue;
        };
        plan.push(PlannedWheelLoss {
            victim_id,
            requested,
            destination: DbHostileDestination::Reserve,
            recipient_id: None,
            clamp: true,
        });
    }
    let mut total = 0_i64;
    let mut count = 0_usize;
    let mut spinner_loss = 0_i64;
    let losses = settle_wheel_losses(
        path,
        guild_id,
        spinner_id,
        WheelMechanic::Recession.code(),
        event_id,
        &plan,
        Some(crate::wheel::HOSTILE_LOSS_MIN_BALANCE),
        now,
    )?;
    for (planned, settled) in losses.settled {
        total = total.saturating_add(settled.applied);
        if settled.applied > 0 {
            count = count.saturating_add(1);
        }
        if planned.victim_id == spinner_id {
            spinner_loss = spinner_loss.saturating_add(settled.applied);
        }
    }
    Ok(RecessionResolution {
        total,
        count,
        self_loss: spinner_loss,
        warnings: losses.warnings,
    })
}

#[derive(Clone, Debug, Default)]
struct HostileResolution {
    /// Numeric value persisted to wheel history.  This intentionally differs
    /// from `total` for effects whose Python history contract records zero
    /// (for example Lightning, Emergency, and a missed Takeover).
    log_result: i64,
    lightning_total: i64,
    lightning_count: usize,
    /// Presentation facts retained from the same settlement pass.  Python's
    /// result embed reports these values; recomputing them after the atomic
    /// hostile-loss pass would race another economy event, so keep the live
    /// receipt snapshot beside the durable result.
    total: i64,
    count: usize,
    missed: bool,
    self_hit: bool,
    victim_id: Option<i64>,
    victim_new_balance: Option<i64>,
    victims: Vec<(i64, i64)>,
    shield_absorbed_total: i64,
    shielded_count: usize,
    /// Net JopaCoin this outcome moved into the spinner's balance: stolen
    /// credits and any fallback credit, less what the spinner lost.
    spinner_delta: i64,
    warnings: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn resolve_hostile_mechanic(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    balance_before: i64,
    mechanic: WheelMechanic,
    event_id: &str,
    config: &WheelSettlementConfig,
    vanity_taxable_ids: &BTreeSet<i64>,
    low_priority_taxable_ids: &BTreeSet<i64>,
    event_win_multiplier: f64,
    visible_member_ids: Option<&BTreeSet<i64>>,
    rng_seed: u64,
    now: i64,
) -> Result<HostileResolution, String> {
    let players = PlayerRepository::new(path);
    // Python parity: every hostile victim pool (heist window, crash top-N,
    // takeover rank, shell/bomb/commune/lightning sweeps) is built from the
    // guild-member visibility-filtered leaderboard
    // (`wheel_outcomes.py::_leaderboard`); an unfiltered read lets departed
    // ex-members consume window slots or be robbed.
    let leaderboard = filter_visible_wheel_leaderboard(
        players
            .get_leaderboard(Some(guild_id), i64::from(i32::MAX), 0)
            .map_err(|error| error.to_string())?,
        visible_member_ids,
    );
    let eligible = leaderboard
        .iter()
        .filter(|row| {
            row.discord_id != Some(user_id)
                && row.jopacoin_balance >= crate::wheel::HOSTILE_LOSS_MIN_BALANCE
        })
        .collect::<Vec<_>>();
    // The seed is frozen with the pending interaction, so a retried
    // settlement re-draws exactly what the first attempt drew.
    let mut rng = fastrand::Rng::with_seed(rng_seed);
    let steal = |victim_id: i64, requested: i64, clamp: bool| PlannedWheelLoss {
        victim_id,
        requested,
        destination: DbHostileDestination::Player,
        recipient_id: Some(user_id),
        clamp,
    };
    let burn = |victim_id: i64, requested: i64| PlannedWheelLoss {
        victim_id,
        requested,
        destination: DbHostileDestination::Burn,
        recipient_id: None,
        clamp: true,
    };
    let scale = config.minigame_jc_delta_scale;
    // Plan every victim first, then settle the whole outcome in one
    // transaction so a failure can never leave it half applied.
    let mut plan = Vec::new();
    match mechanic {
        WheelMechanic::RedShell => {
            if let Some(victim) = players
                .get_player_above(user_id, Some(guild_id), Some(50))
                .map_err(|error| error.to_string())?
                && let Some(victim_id) = victim.discord_id
            {
                let roll = LossRoll {
                    percentage: 0.02 + rng.f64() * (0.0924 - 0.02),
                    flat: rng.i64(2..14),
                };
                plan.push(steal(
                    victim_id,
                    requested_percentage_or_flat(victim.jopacoin_balance, roll, scale),
                    false,
                ));
            }
        }
        WheelMechanic::BlueShell => {
            if let Some(victim) = leaderboard.first()
                && let Some(victim_id) = victim.discord_id
            {
                let roll = if victim_id == user_id {
                    LossRoll {
                        percentage: 0.02 + rng.f64() * (0.07 - 0.02),
                        flat: rng.i64(4..21),
                    }
                } else {
                    LossRoll {
                        percentage: 0.02 + rng.f64() * (0.1016 - 0.02),
                        flat: rng.i64(4..30),
                    }
                };
                let requested = requested_percentage_or_flat(victim.jopacoin_balance, roll, scale);
                plan.push(if victim_id == user_id {
                    PlannedWheelLoss {
                        victim_id,
                        requested,
                        destination: DbHostileDestination::Reserve,
                        recipient_id: None,
                        clamp: false,
                    }
                } else {
                    steal(victim_id, requested, false)
                });
            }
        }
        WheelMechanic::BananaPeel => {
            if let Some(victim) = players
                .get_player_below(user_id, Some(guild_id))
                .map_err(|error| error.to_string())?
                && let Some(victim_id) = victim.discord_id
            {
                // Python: min(scale_minigame_jc_delta(randint(...)), balance).
                plan.push(burn(
                    victim_id,
                    crate::wheel::hostile_flat_loss(
                        rng.i64(WHEEL_BANANA_PEEL_LOSS_MIN..WHEEL_BANANA_PEEL_LOSS_MAX + 1),
                        victim.jopacoin_balance,
                        scale,
                    ),
                ));
            }
        }
        WheelMechanic::GreenShell => {
            if let Some(victim) = eligible.get(rng.usize(..eligible.len().max(1)))
                && let Some(victim_id) = victim.discord_id
            {
                // Python: min(scale_minigame_jc_delta(randint(...)), balance).
                plan.push(steal(
                    victim_id,
                    crate::wheel::hostile_flat_loss(
                        rng.i64(WHEEL_GREEN_SHELL_STEAL_MIN..WHEEL_GREEN_SHELL_STEAL_MAX + 1),
                        victim.jopacoin_balance,
                        scale,
                    ),
                    false,
                ));
            }
        }
        WheelMechanic::BombOmb => {
            let mut sampled = eligible.clone();
            rng.shuffle(&mut sampled);
            for victim in sampled.iter().take(WHEEL_BOMB_OMB_VICTIM_COUNT) {
                if let Some(victim_id) = victim.discord_id {
                    // Python: min(scale_minigame_jc_delta(randint(...)), balance).
                    plan.push(burn(
                        victim_id,
                        crate::wheel::hostile_flat_loss(
                            rng.i64(
                                WHEEL_BOMB_OMB_VICTIM_LOSS_MIN..WHEEL_BOMB_OMB_VICTIM_LOSS_MAX + 1,
                            ),
                            victim.jopacoin_balance,
                            scale,
                        ),
                    ));
                }
            }
        }
        WheelMechanic::LightningBolt => {
            let tax_rate = 0.02 + rng.f64() * (0.0627 - 0.02);
            // Lightning is the one server-wide tax that includes the
            // spinner. Python's leaderboard pass intentionally charges every
            // account at/above the hostile floor, including self.
            for victim in leaderboard
                .iter()
                .filter(|player| player.jopacoin_balance >= crate::wheel::HOSTILE_LOSS_MIN_BALANCE)
            {
                if let Some(victim_id) = victim.discord_id {
                    plan.push(PlannedWheelLoss {
                        victim_id,
                        requested: crate::wheel::hostile_percentage_loss(
                            victim.jopacoin_balance,
                            tax_rate,
                            scale,
                        ),
                        destination: DbHostileDestination::Reserve,
                        recipient_id: None,
                        clamp: true,
                    });
                }
            }
        }
        WheelMechanic::Commune | WheelMechanic::TrickleDown => {
            let tax_rate = 0.02 + rng.f64() * (0.05 - 0.02);
            for victim in &eligible {
                if let Some(victim_id) = victim.discord_id {
                    let requested = if mechanic == WheelMechanic::Commune {
                        1
                    } else {
                        crate::wheel::hostile_percentage_loss(
                            victim.jopacoin_balance,
                            tax_rate,
                            scale,
                        )
                    };
                    plan.push(steal(victim_id, requested, true));
                }
            }
        }
        WheelMechanic::Heist => {
            // Python parity: the window is the poorest thirty accounts with
            // any positive balance (spinner included), and only then filters
            // to stealable victims (`wheel_outcomes.py::_heist`). Filtering by
            // the hostile floor first would target the richest players
            // instead. The leaderboard is sorted exactly opposite to
            // `get_leaderboard_bottom`, so reversing reproduces its order.
            let heist_window = leaderboard
                .iter()
                .rev()
                .filter(|player| player.jopacoin_balance >= 1)
                .take(30)
                .filter(|player| {
                    player.discord_id != Some(user_id)
                        && player.jopacoin_balance >= crate::wheel::HOSTILE_LOSS_MIN_BALANCE
                })
                .collect::<Vec<_>>();
            for victim in heist_window {
                if let Some(victim_id) = victim.discord_id {
                    // Python truncates before scaling:
                    // scale(max(1, int(balance * uniform(0.05, 0.12)))).
                    plan.push(steal(
                        victim_id,
                        crate::wheel::hostile_percentage_loss(
                            victim.jopacoin_balance,
                            0.05 + rng.f64() * (0.12 - 0.05),
                            scale,
                        ),
                        false,
                    ));
                }
            }
        }
        WheelMechanic::MarketCrash => {
            for victim in leaderboard.iter().take(3) {
                if let Some(victim_id) = victim.discord_id
                    && victim_id != user_id
                    && victim.jopacoin_balance >= crate::wheel::HOSTILE_LOSS_MIN_BALANCE
                {
                    // Python truncates before scaling:
                    // scale(max(1, int(balance * uniform(0.08, 0.15)))).
                    plan.push(steal(
                        victim_id,
                        crate::wheel::hostile_percentage_loss(
                            victim.jopacoin_balance,
                            0.08 + rng.f64() * (0.15 - 0.08),
                            scale,
                        ),
                        false,
                    ));
                }
            }
        }
        WheelMechanic::HostileTakeover => {
            if let Some(victim) = leaderboard.get(3)
                && let Some(victim_id) = victim.discord_id
                && victim.jopacoin_balance >= crate::wheel::HOSTILE_LOSS_MIN_BALANCE
            {
                // Python truncates before scaling:
                // scale(max(1, int(balance * uniform(0.08, 0.15)))).
                plan.push(steal(
                    victim_id,
                    crate::wheel::hostile_percentage_loss(
                        victim.jopacoin_balance,
                        0.08 + rng.f64() * (0.15 - 0.08),
                        scale,
                    ),
                    false,
                ));
            }
        }
        WheelMechanic::Emergency => {
            for victim in &leaderboard {
                if let Some(victim_id) = victim.discord_id
                    && victim.jopacoin_balance >= crate::wheel::HOSTILE_LOSS_MIN_BALANCE
                {
                    plan.push(burn(
                        victim_id,
                        cama_domain::economy_scaling::scale_minigame_jc_delta(20.0, scale),
                    ));
                }
            }
        }
        WheelMechanic::Decay => {
            for (rank, victim) in leaderboard.iter().take(4).enumerate() {
                if let Some(victim_id) = victim.discord_id
                    && victim_id != user_id
                    && victim.jopacoin_balance >= crate::wheel::HOSTILE_LOSS_MIN_BALANCE
                {
                    plan.push(steal(
                        victim_id,
                        cama_domain::economy_scaling::scale_minigame_jc_delta(
                            if rank == 3 { 80.0 } else { 60.0 },
                            scale,
                        ),
                        true,
                    ));
                }
            }
        }
        _ => {}
    }
    let SettledWheelLosses { settled, warnings } = settle_wheel_losses(
        path,
        guild_id,
        user_id,
        mechanic.code(),
        event_id,
        &plan,
        Some(50),
        now,
    )?;

    let mut total = 0_i64;
    let mut applied_count = 0_usize;
    let mut shield_absorbed_total = 0_i64;
    let mut shielded_count = 0_usize;
    let mut spinner_delta = 0_i64;
    for (planned, settlement) in &settled {
        total = total.saturating_add(settlement.applied);
        if settlement.applied > 0 {
            applied_count = applied_count.saturating_add(1);
        }
        if settlement.absorbed > 0 {
            shield_absorbed_total = shield_absorbed_total.saturating_add(settlement.absorbed);
            shielded_count = shielded_count.saturating_add(1);
        }
        if planned.recipient_id == Some(user_id) {
            spinner_delta = spinner_delta.saturating_add(settlement.applied);
        }
        if planned.victim_id == user_id {
            spinner_delta = spinner_delta.saturating_sub(settlement.applied);
        }
    }
    let hit =
        |(planned, settlement): &(
            PlannedWheelLoss,
            cama_db::shop_runtime::HostileLossSettlement,
        )| { (settlement.applied > 0).then_some((planned.victim_id, settlement.applied)) };
    let primary = settled
        .first()
        .filter(|(_, settlement)| settlement.applied > 0);
    let mut log_result = 0_i64;
    let mut primary_victim_id = None;
    let mut victim_new_balance = None;
    let mut victims = Vec::new();
    let mut self_hit = false;
    let mut missed = false;
    // Heist, Market Crash, and a missed Takeover pay a consolation credit
    // when nothing was stolen. It carries its own exact-once marker.
    let mut fallback_credit = |base: f64, reason: &str| -> Result<i64, String> {
        let reward = apply_gamba_event_multiplier(
            cama_domain::economy_scaling::scale_minigame_jc_delta(base, scale),
            event_win_multiplier,
            1.0,
        );
        let receipt = credit_wheel_income(
            path,
            guild_id,
            user_id,
            balance_before,
            reward,
            config,
            event_id,
            reason,
            vanity_taxable_ids,
            low_priority_taxable_ids,
        )?;
        spinner_delta = spinner_delta.saturating_add(receipt.balance_delta);
        Ok(receipt.net)
    };
    match mechanic {
        WheelMechanic::RedShell | WheelMechanic::GreenShell => {
            log_result = total;
            if let Some((planned, settlement)) = primary {
                primary_victim_id = Some(planned.victim_id);
                if mechanic == WheelMechanic::RedShell {
                    victim_new_balance = Some(settlement.victim_balance_after);
                }
            }
        }
        WheelMechanic::BlueShell => {
            self_hit = plan
                .first()
                .is_some_and(|planned| planned.victim_id == user_id);
            log_result = if self_hit { -total } else { total };
            if !self_hit && let Some((planned, settlement)) = primary {
                primary_victim_id = Some(planned.victim_id);
                victim_new_balance = Some(settlement.victim_balance_after);
            }
        }
        WheelMechanic::BananaPeel => {
            primary_victim_id = primary.map(|(planned, _)| planned.victim_id);
        }
        WheelMechanic::BombOmb => {
            victims = settled.iter().filter_map(hit).collect();
        }
        WheelMechanic::LightningBolt => {
            victims = settled.iter().filter_map(hit).take(3).collect();
        }
        WheelMechanic::Commune | WheelMechanic::TrickleDown => {
            victims = settled.iter().filter_map(hit).take(3).collect();
            log_result = total;
        }
        WheelMechanic::Heist | WheelMechanic::MarketCrash => {
            victims = settled.iter().filter_map(hit).take(3).collect();
            if total == 0 {
                total = if mechanic == WheelMechanic::Heist {
                    fallback_credit(20.0, "gamba heist fallback credit")?
                } else {
                    fallback_credit(25.0, "gamba market crash fallback credit")?
                };
            }
            log_result = total;
        }
        WheelMechanic::HostileTakeover => {
            if let Some((planned, settlement)) = primary {
                primary_victim_id = Some(planned.victim_id);
                victim_new_balance = Some(settlement.victim_balance_after);
            }
            missed = total == 0;
            if missed {
                total = fallback_credit(40.0, "gamba hostile takeover fallback credit")?;
            }
            // Python records a missed Takeover as zero even though the
            // fallback consolation credit is still applied to the wallet.
            log_result = if missed { 0 } else { total };
        }
        _ => {}
    }
    if matches!(
        mechanic,
        WheelMechanic::RedShell
            | WheelMechanic::BlueShell
            | WheelMechanic::BananaPeel
            | WheelMechanic::GreenShell
            | WheelMechanic::BombOmb
    ) && total == 0
    {
        missed = true;
    }
    victims.sort_by(|left, right| right.1.cmp(&left.1));
    let lightning_count = if mechanic == WheelMechanic::LightningBolt {
        applied_count
    } else {
        0
    };
    Ok(HostileResolution {
        log_result,
        lightning_total: if mechanic == WheelMechanic::LightningBolt {
            total
        } else {
            0
        },
        lightning_count,
        total,
        count: applied_count,
        missed,
        self_hit,
        victim_id: primary_victim_id,
        victim_new_balance,
        victims,
        shield_absorbed_total,
        shielded_count,
        spinner_delta,
        warnings,
    })
}

fn hostile_result_note(
    mechanic: WheelMechanic,
    outcome: &HostileResolution,
    config: &WheelSettlementConfig,
    player_names: &BTreeMap<i64, String>,
) -> String {
    let player_name = |player_id: i64| {
        player_names
            .get(&player_id)
            .cloned()
            .unwrap_or_else(|| UNKNOWN_PLAYER.to_owned())
    };
    let victim_name = outcome.victim_id.map(player_name);
    let note = match mechanic {
        WheelMechanic::RedShell if outcome.missed => {
            "The Red Shell circles the track but finds no eligible target!".to_owned()
        }
        WheelMechanic::RedShell => format!(
            "💥 Red Shell locked onto **{}**! You stole **{}** {}.\n\n*Victim's new balance: **{}** {}*",
            victim_name.as_deref().unwrap_or("the player above"),
            outcome.total,
            JOPACOIN_EMOTE,
            outcome.victim_new_balance.unwrap_or_default(),
            JOPACOIN_EMOTE,
        ),
        WheelMechanic::BlueShell if outcome.missed => {
            "The Blue Shell circles the track but finds no target!".to_owned()
        }
        WheelMechanic::BlueShell if outcome.self_hit => format!(
            "💥 The Blue Shell targets the leader — **that's you**! You lost **{}** {}.",
            outcome.total, JOPACOIN_EMOTE,
        ),
        WheelMechanic::BlueShell => format!(
            "💥 Blue Shell targets the leader: **{}**! You stole **{}** {}.\n\n*Victim's new balance: **{}** {}*",
            victim_name.as_deref().unwrap_or("the richest player"),
            outcome.total,
            JOPACOIN_EMOTE,
            outcome.victim_new_balance.unwrap_or_default(),
            JOPACOIN_EMOTE,
        ),
        WheelMechanic::LightningBolt => {
            let victims = outcome
                .victims
                .iter()
                .map(|(player_id, amount)| {
                    format!("⚡ **{}** lost **{amount}** JC\n", player_name(*player_id))
                })
                .collect::<String>();
            format!(
                "Lightning struck **{}** player(s) for a total of **{}** {}. All funds went to the Jopacoin Reserve.\n{}",
                outcome.lightning_count, outcome.lightning_total, JOPACOIN_EMOTE, victims,
            )
        }
        WheelMechanic::Emergency => format!(
            "Economic crisis: **{}** player(s) each lost up to **{}** {}. Total drained: **{}** {} (vanished).",
            outcome.count,
            cama_domain::economy_scaling::scale_minigame_jc_delta(
                20.0,
                config.minigame_jc_delta_scale,
            ),
            JOPACOIN_EMOTE,
            outcome.total,
            JOPACOIN_EMOTE,
        ),
        WheelMechanic::Commune => format!(
            "**{}** player(s) donated 1 JC. You received **+{}** {} from the collective.",
            outcome.count, outcome.total, JOPACOIN_EMOTE,
        ),
        WheelMechanic::Heist => {
            if outcome.count == 0 {
                format!(
                    "**HEIST**\n\nYou cased the joint... but the bottom 30 are already broke.\n\n*Consolation prize: **+{}** {}.*",
                    outcome.total, JOPACOIN_EMOTE
                )
            } else {
                format!(
                    "**HEIST**\n\n💰 You robbed **{}** players at the bottom of the ladder!\nTotal stolen: **{}** {}\n\n*Crime pays — when you're already on top.*",
                    outcome.count, outcome.total, JOPACOIN_EMOTE
                )
            }
        }
        WheelMechanic::MarketCrash => {
            if outcome.count == 0 {
                format!(
                    "No eligible whales were found. Consolation prize: **+{}** {}.",
                    outcome.total, JOPACOIN_EMOTE
                )
            } else {
                format!(
                    "Taxed **{}** fellow top-3 players for **{}** {} total.",
                    outcome.count, outcome.total, JOPACOIN_EMOTE
                )
            }
        }
        WheelMechanic::TrickleDown => {
            if outcome.count == 0 {
                "There was no one else to tax.".to_owned()
            } else {
                format!(
                    "Taxed **{}** players for **{}** {} total.",
                    outcome.count, outcome.total, JOPACOIN_EMOTE
                )
            }
        }
        WheelMechanic::HostileTakeover if outcome.missed => format!(
            "Rank #4 was broke or absent. Consolation prize: **+{}** {}.",
            outcome.total, JOPACOIN_EMOTE,
        ),
        WheelMechanic::HostileTakeover => format!(
            "Corporate raid on **{}** (rank #4)! Seized **{}** {}.",
            victim_name.as_deref().unwrap_or("rank #4"),
            outcome.total,
            JOPACOIN_EMOTE,
        ),
        WheelMechanic::BananaPeel if outcome.missed => {
            "You dropped a banana peel, but no one was riding your bumper.".to_owned()
        }
        WheelMechanic::BananaPeel => format!(
            "**{}** (ranked just below you) slipped on your peel and lost **{}** {} — burned into the void.",
            victim_name.as_deref().unwrap_or("the player behind you"),
            outcome.total,
            JOPACOIN_EMOTE,
        ),
        WheelMechanic::GreenShell if outcome.missed => {
            "The shell skittered off the track — no one was around to clip.".to_owned()
        }
        WheelMechanic::GreenShell => format!(
            "You launched a green shell at **{}** and stole **{}** {}.",
            victim_name.as_deref().unwrap_or("someone"),
            outcome.total,
            JOPACOIN_EMOTE,
        ),
        WheelMechanic::BombOmb if outcome.missed => {
            "The bomb-omb rolled into an empty room and fizzled.".to_owned()
        }
        WheelMechanic::BombOmb => {
            let victims = outcome
                .victims
                .iter()
                .map(|(player_id, amount)| {
                    format!(
                        "💥 **{}** lost **{amount}** {}\n",
                        player_name(*player_id),
                        JOPACOIN_EMOTE
                    )
                })
                .collect::<String>();
            format!(
                "KABOOM!\n{}Total burned: **{}** {} (vanished, not banked).",
                victims, outcome.total, JOPACOIN_EMOTE
            )
        }
        WheelMechanic::Decay => format!(
            "The top 3 were targeted for {} {} each and rank #4 for {} {}. The Swamp claims what it is owed.",
            cama_domain::economy_scaling::scale_minigame_jc_delta(
                60.0,
                config.minigame_jc_delta_scale,
            ),
            JOPACOIN_EMOTE,
            cama_domain::economy_scaling::scale_minigame_jc_delta(
                80.0,
                config.minigame_jc_delta_scale,
            ),
            JOPACOIN_EMOTE,
        ),
        _ => String::new(),
    };
    if outcome.shield_absorbed_total > 0 {
        format!(
            "{note}\n\n🌾 White Mana Shields: **{}** shield activation(s) absorbed **{}** {}.",
            outcome.shielded_count, outcome.shield_absorbed_total, JOPACOIN_EMOTE,
        )
    } else {
        note
    }
}

fn filter_visible_wheel_leaderboard(
    players: Vec<Player>,
    visible_member_ids: Option<&BTreeSet<i64>>,
) -> Vec<Player> {
    visible_member_ids.map_or(players.clone(), |member_ids| {
        players
            .iter()
            .filter(|row| row.discord_id.is_some_and(|id| member_ids.contains(&id)))
            .cloned()
            .collect()
    })
}

fn visible_wheel_positive_balance(
    players: &[Player],
    visible_member_ids: Option<&BTreeSet<i64>>,
) -> Option<i64> {
    visible_member_ids.map(|member_ids| {
        players
            .iter()
            .filter(|row| {
                row.discord_id.is_some_and(|id| member_ids.contains(&id))
                    && row.jopacoin_balance > 0
            })
            .map(|row| row.jopacoin_balance)
            .sum()
    })
}

fn wheel_bankrupt_preview(wedges: &[crate::wheel::WheelWedge]) -> String {
    if wedges.is_empty() {
        return "Blue mana reveals: no wedges available".to_owned();
    }
    let sample_count = wedges.len().min(3);
    let mut indices = Vec::with_capacity(sample_count);
    while indices.len() < sample_count {
        let index = fastrand::usize(..wedges.len());
        if !indices.contains(&index) {
            indices.push(index);
        }
    }
    let preview = indices
        .into_iter()
        .filter_map(|index| wedges.get(index))
        .map(|wedge| match wedge.value {
            WheelValue::Numeric(value) if value > 0 => format!("{value} JC"),
            _ => wedge.label.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("Blue mana reveals: {preview}")
}

/// Victims one outcome settled, and those the repository rejected.
#[derive(Debug, Default)]
struct SettledWheelLosses {
    settled: Vec<(PlannedWheelLoss, HostileLossSettlement)>,
    warnings: Vec<String>,
}

/// The settled facts of one landed wedge, for the result embed and Neon.
#[derive(Clone, Debug)]
pub struct WheelOutcomeSummary {
    pub landed: WheelWedge,
    pub kind: WheelKind,
    pub resolved: WheelValue,
    pub cooldown_outcome: CooldownOutcome,
    pub next_spin_at: i64,
    /// Balance the outcome was resolved against.
    pub balance_before: i64,
    pub balance: i64,
    pub blood_pact_skim: i64,
    pub extra_note: String,
    pub direct_loss_settled: bool,
    pub income: Option<IncomeAwardReceipt>,
    pub neon_lightning: Option<(i64, usize)>,
    pub bonus_spin: bool,
    /// Non-fatal problems the provider should log.
    pub warnings: Vec<String>,
}

impl WheelOutcomeSummary {
    /// The outcome note, Lose-Turn notice, and Blood Pact notice in order.
    #[must_use]
    pub fn presentation_note(&self) -> String {
        format!(
            "{}{}{}",
            self.extra_note,
            self.lose_turn_note(),
            self.blood_pact_note()
        )
    }

    #[must_use]
    pub const fn lose_turn_note(&self) -> &'static str {
        if matches!(self.cooldown_outcome, CooldownOutcome::LoseTurn) {
            "\n⏳ Lose Turn: your next spin is delayed for five days."
        } else {
            ""
        }
    }

    #[must_use]
    pub fn blood_pact_note(&self) -> String {
        if self.blood_pact_skim > 0 {
            format!(
                "\n🩸 Blood Pact skimmed **{}** {JOPACOIN_EMOTE} from this payout.",
                self.blood_pact_skim
            )
        } else {
            String::new()
        }
    }

    /// The landed loss Neon reacts to, after any pardon or shield.
    #[must_use]
    pub const fn neon_wheel_loss(&self) -> Option<i64> {
        match self.resolved {
            WheelValue::Numeric(value) if value < 0 => Some(value),
            _ => None,
        }
    }
}

/// Inputs every settled spin records after its outcome resolved.
struct WheelSpinTail<'a> {
    path: &'a Path,
    guild_id: i64,
    user_id: i64,
    now: i64,
    kind: WheelKind,
    bonus_spin: bool,
    last_regular_spin: Option<i64>,
    event_id: &'a str,
    cooldown_outcome: CooldownOutcome,
    logged: i64,
    earning: i64,
    outcome_code: &'a str,
    minigame_scale: f64,
    /// Balance reported when the post-outcome read finds no player.
    fallback_balance: i64,
}

/// What the shared tail recorded.
struct WheelTailReceipt {
    next_spin_at: i64,
    balance: i64,
    blood_pact_skim: i64,
    warnings: Vec<String>,
}

/// The one post-spin tail for direct, exploding, and interactive spins:
/// record the cooldown, skim Blood Pact from the outcome's own earning, log
/// the spin, note pet activity, and schedule the wheel-ready reminder.
fn finish_wheel_spin(
    tail: &WheelSpinTail<'_>,
    metadata: impl FnOnce(i64, i64) -> serde_json::Value,
    reminders: &dyn WheelReminderPort,
) -> Result<WheelTailReceipt, String> {
    let players = PlayerRepository::new(tail.path);
    let next = cooldown_after_resolution(
        tail.now,
        tail.bonus_spin,
        tail.last_regular_spin,
        tail.cooldown_outcome,
    );
    if let Some(replacement) = next.replacement_last_spin {
        players
            .set_last_wheel_spin(tail.user_id, Some(tail.guild_id), replacement)
            .map_err(|error| error.to_string())?;
    }
    let balance = players
        .get_by_id(tail.user_id, Some(tail.guild_id))
        .map_err(|error| error.to_string())?
        .map_or(tail.fallback_balance, |row| row.jopacoin_balance);
    let mut warnings = Vec::new();
    let (blood_pact_skim, balance) = match apply_wheel_blood_pact_skim(
        tail.path,
        tail.guild_id,
        tail.user_id,
        tail.earning,
        tail.event_id,
        tail.now,
        tail.minigame_scale,
    ) {
        Ok(result) => result,
        Err(error) => {
            warnings.push(format!("failed to apply wheel Blood Pact skim: {error}"));
            (0, balance)
        }
    };
    log_wheel_spin(
        tail.path,
        tail.user_id,
        tail.guild_id,
        tail.logged,
        tail.now,
        tail.kind,
        tail.bonus_spin,
        tail.outcome_code,
        tail.event_id,
        metadata(next.next_spin_at, blood_pact_skim),
    )?;
    record_betting_pet_activity(
        tail.path,
        tail.user_id,
        tail.guild_id,
        PetActivity::WheelSpun,
        &format!("wheel:{}", tail.event_id),
        tail.now,
    );
    if next.schedule_reminder
        && let Err(error) = reminders.schedule_wheel(tail.user_id, tail.guild_id, next.next_spin_at)
    {
        warnings.push(format!("failed to schedule the wheel reminder: {error}"));
    }
    Ok(WheelTailReceipt {
        next_spin_at: next.next_spin_at,
        balance,
        blood_pact_skim,
        warnings,
    })
}

/// Inputs shared by the direct and interactive settlement of one wedge.
struct LandedWheelSettlement<'a> {
    path: &'a Path,
    guild_id: i64,
    user_id: i64,
    now: i64,
    kind: WheelKind,
    effects: &'a ManaEffects,
    config: &'a WheelSettlementConfig,
    event_id: &'a str,
    event_win_multiplier: f64,
    event_loss_multiplier: f64,
    bonus_spin: bool,
    last_regular_spin: Option<i64>,
    insurance_activated: bool,
    interactive: bool,
}

/// Run the shared tail for an already-resolved wedge.
fn settle_landed_wedge(
    settlement: &LandedWheelSettlement<'_>,
    landed: &WheelWedge,
    balance_before: i64,
    resolution: WheelResolution,
    reminders: &dyn WheelReminderPort,
) -> Result<WheelOutcomeSummary, String> {
    let WheelResolution {
        resolved,
        logged,
        cooldown_outcome,
        direct_loss_settled,
        extra_note,
        income,
        neon_lightning,
        earning,
        mut warnings,
    } = resolution;
    let outcome_code = canonical_outcome_code(landed, resolved, settlement.kind);
    let tail = finish_wheel_spin(
        &WheelSpinTail {
            path: settlement.path,
            guild_id: settlement.guild_id,
            user_id: settlement.user_id,
            now: settlement.now,
            kind: settlement.kind,
            bonus_spin: settlement.bonus_spin,
            last_regular_spin: settlement.last_regular_spin,
            event_id: settlement.event_id,
            cooldown_outcome,
            logged,
            earning,
            outcome_code: &outcome_code,
            minigame_scale: settlement.config.minigame_jc_delta_scale,
            fallback_balance: balance_before,
        },
        |next_spin_at, blood_pact_skim| {
            let mut metadata = json!({
                "wedge_label": landed.label,
                "resolved_value": wheel_value_json(resolved),
                "next_spin_at": next_spin_at,
                "mana": settlement.effects.color,
                "mana_insurance_activated": settlement.insurance_activated,
                "blood_pact_skim": blood_pact_skim,
                "event_win_multiplier": settlement.event_win_multiplier,
                "event_loss_multiplier": settlement.event_loss_multiplier,
            });
            if settlement.interactive {
                metadata["interactive"] = json!(true);
            }
            metadata
        },
        reminders,
    )?;
    warnings.extend(tail.warnings);
    Ok(WheelOutcomeSummary {
        landed: landed.clone(),
        kind: settlement.kind,
        resolved,
        cooldown_outcome,
        next_spin_at: tail.next_spin_at,
        balance_before,
        balance: tail.balance,
        blood_pact_skim: tail.blood_pact_skim,
        extra_note,
        direct_loss_settled,
        income,
        neon_lightning,
        bonus_spin: settlement.bonus_spin,
        warnings,
    })
}

/// A direct `/gamba` request.
pub struct WheelSpinRequest<'a> {
    pub path: &'a Path,
    pub guild_id: i64,
    pub user_id: i64,
    pub now: i64,
    pub config: &'a WheelSettlementConfig,
    pub event_win_multiplier: f64,
    pub event_loss_multiplier: f64,
    pub vanity_taxable_ids: &'a BTreeSet<i64>,
    pub low_priority_taxable_ids: &'a BTreeSet<i64>,
    pub admin_bypass: bool,
    pub bonus_spin: bool,
    pub visible_member_ids: Option<&'a BTreeSet<i64>>,
    pub player_names: &'a dyn WheelPlayerNameSource,
    pub reminders: &'a dyn WheelReminderPort,
}

/// An exploding wheel's settled credit.
#[derive(Clone, Debug)]
pub struct ExplodedWheelSpin {
    /// The explosion credit, net of the Blood Pact skim.
    pub receipt: IncomeAwardReceipt,
    pub blood_pact_skim: i64,
    pub next_spin_at: i64,
    pub bonus_spin: bool,
    pub warnings: Vec<String>,
}

/// An interactive wheel the player (or the town) still has to decide.
#[derive(Clone, Debug)]
pub struct PendingWheelPlan {
    pub key: String,
    pub interaction: WheelInteractionKind,
    pub options: Vec<WheelOption>,
    pub wedges: Vec<WheelWedge>,
    pub spin_kind: WheelKind,
    pub balance_before: i64,
    pub effects: ManaEffects,
    pub event_id: String,
    pub event_win_multiplier: f64,
    pub event_loss_multiplier: f64,
    pub bonus_spin: bool,
    pub last_regular_spin: Option<i64>,
    /// Blue mana's bankrupt-wheel preview text.
    pub blue_preview: Option<String>,
}

/// A direct spin that landed and settled.
#[derive(Clone, Debug)]
pub struct SettledWheelSpin {
    pub wedges: Vec<WheelWedge>,
    pub landed_index: usize,
    pub outcome: WheelOutcomeSummary,
    pub blue_preview: Option<String>,
    /// The visible top balances a Golden wheel announces, as
    /// `(discord_id, balance)` in rank order.
    pub golden_leaderboard: Option<Vec<(i64, i64)>>,
}

#[derive(Clone, Debug)]
pub enum WheelSpinOutcome {
    /// Admission refused the spin (unregistered or on cooldown).
    Rejected(String),
    Exploded(Box<ExplodedWheelSpin>),
    Pending(Box<PendingWheelPlan>),
    Settled(Box<SettledWheelSpin>),
}

/// Admit, select, and settle one direct `/gamba` spin. Runs on a blocking
/// worker.
pub fn spin_wheel(request: &WheelSpinRequest<'_>) -> Result<WheelSpinOutcome, String> {
    let WheelSpinRequest {
        path,
        guild_id,
        user_id,
        now,
        config,
        event_win_multiplier,
        event_loss_multiplier,
        vanity_taxable_ids,
        low_priority_taxable_ids,
        admin_bypass,
        bonus_spin,
        visible_member_ids,
        player_names,
        reminders,
    } = *request;
    let players = PlayerRepository::new(path);
    let Some(player) = players
        .get_by_id(user_id, Some(guild_id))
        .map_err(|error| error.to_string())?
    else {
        return Ok(WheelSpinOutcome::Rejected(
            "You need to /player register before you can spin the wheel.".to_owned(),
        ));
    };
    let last_regular_spin = bonus_spin
        .then(|| players.get_last_wheel_spin(user_id, Some(guild_id)))
        .transpose()
        .map_err(|error| error.to_string())?
        .flatten();
    if bonus_spin {
        // Dig bonuses are free and deliberately leave the persisted regular
        // cooldown untouched.  Keep the prior timestamp for the final copy.
    } else if admin_bypass {
        players
            .set_last_wheel_spin(user_id, Some(guild_id), now)
            .map_err(|error| error.to_string())?;
    } else {
        match players
            .try_claim_wheel_spin(user_id, Some(guild_id), now, wheel_window_start_at(now))
            .map_err(|error| error.to_string())?
        {
            WheelSpinClaim::MissingPlayer => {
                return Ok(WheelSpinOutcome::Rejected(
                    "You need to /player register before you can spin the wheel.".to_owned(),
                ));
            }
            WheelSpinClaim::Cooldown { last_spin } => {
                return Ok(WheelSpinOutcome::Rejected(wheel_cooldown_message(
                    now, last_spin,
                )));
            }
            WheelSpinClaim::Claimed { .. } => {}
        }
    }

    let raw_leaderboard = players
        .get_leaderboard(Some(guild_id), i64::from(i32::MAX), 0)
        .map_err(|error| error.to_string())?;
    let leaderboard = filter_visible_wheel_leaderboard(raw_leaderboard.clone(), visible_member_ids);
    let visible_positive_balance =
        visible_wheel_positive_balance(&raw_leaderboard, visible_member_ids);
    let top_ids = leaderboard
        .iter()
        .take(crate::wheel::WHEEL_GOLDEN_TOP_N)
        .filter_map(|row| row.discord_id)
        .collect::<Vec<_>>();
    let kind = select_wheel_kind(user_id, player.jopacoin_balance, bonus_spin, &top_ids);
    let golden_leaderboard = (kind == WheelKind::Golden && !bonus_spin).then(|| {
        leaderboard
            .iter()
            .take(crate::wheel::WHEEL_GOLDEN_TOP_N)
            .filter_map(|row| row.discord_id.map(|id| (id, row.jopacoin_balance)))
            .collect::<Vec<_>>()
    });
    let policy = WheelEconomyPolicy {
        minigame_scale: config.minigame_jc_delta_scale,
        regular_target_ev: config.wheel_target_ev,
        bankrupt_target_ev: config.wheel_bankrupt_target_ev,
        golden_target_ev: config.wheel_golden_target_ev,
    };
    let effects = active_mana_effects(path, user_id, Some(guild_id), now)?;
    let mut wedges = if kind == WheelKind::Golden {
        let top_balances = leaderboard
            .iter()
            .take(crate::wheel::WHEEL_GOLDEN_TOP_N)
            .filter_map(|row| {
                (row.discord_id != Some(user_id) && row.jopacoin_balance > 0)
                    .then_some(row.jopacoin_balance)
            })
            .collect::<Vec<_>>();
        let rank_next = leaderboard
            .get(crate::wheel::WHEEL_GOLDEN_TOP_N)
            .filter(|row| row.jopacoin_balance > 0)
            .map(|row| row.jopacoin_balance);
        let total_positive = GoldenWheelRepository::new(path)
            .get_total_positive_balance(Some(guild_id))
            .map_err(|error| error.to_string())?;
        let bottom = GoldenWheelRepository::new(path)
            .get_leaderboard_bottom(Some(guild_id), 30, 1)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|row| row.discord_id != user_id)
            .filter(|row| {
                visible_member_ids.is_none_or(|member_ids| member_ids.contains(&row.discord_id))
            })
            .map(|row| row.balance)
            .collect::<Vec<_>>();
        compute_live_golden_wedges(
            LiveGoldenEconomy {
                spinner_balance: player.jopacoin_balance,
                other_top_balances: &top_balances,
                rank_next_balance: rank_next,
                total_positive_balance: total_positive,
                bottom_player_balances: &bottom,
            },
            policy,
        )
    } else {
        get_wheel_wedges(kind == WheelKind::Bankrupt, false, policy)
    };
    apply_mana_wheel_modifiers(&mut wedges, &effects, kind);
    if wedges.is_empty() {
        return Err("wheel has no wedges".to_owned());
    }
    // `fastrand` keeps the adapter dependency-free while preserving Python's
    // uniform random wedge selection. The persisted claim is the idempotency
    // boundary; a process restart cannot replay a claimed spin.
    let index = fastrand::usize(..wedges.len());
    let mut landed = wedges
        .get(index)
        .cloned()
        .ok_or_else(|| "wheel has no wedges".to_owned())?;
    let event_id = format!("{now}:{user_id}:{:016x}", fastrand::u64(..));

    if fastrand::f64() < crate::wheel::WHEEL_EXPLOSION_CHANCE {
        let explosion = resolve_explosion(
            bonus_spin,
            player.jopacoin_balance,
            event_win_multiplier,
            config.minigame_jc_delta_scale,
        );
        let mut receipt = credit_wheel_income(
            path,
            guild_id,
            user_id,
            player.jopacoin_balance,
            explosion.credited_reward,
            config,
            &event_id,
            "EXPLOSION",
            vanity_taxable_ids,
            low_priority_taxable_ids,
        )?;
        let tail = finish_wheel_spin(
            &WheelSpinTail {
                path,
                guild_id,
                user_id,
                now,
                kind,
                bonus_spin,
                last_regular_spin,
                event_id: &event_id,
                cooldown_outcome: CooldownOutcome::Other,
                logged: explosion.log_result,
                earning: receipt.balance_delta,
                outcome_code: "EXPLOSION",
                minigame_scale: config.minigame_jc_delta_scale,
                fallback_balance: receipt.balance_after,
            },
            |_, blood_pact_skim| {
                json!({
                    "gross_reward": explosion.gross_reward,
                    "credited_reward": explosion.credited_reward,
                    "garnished_amount": receipt.garnished,
                    "bankruptcy_penalty": receipt.bankruptcy_penalty,
                    "vanity_tax": receipt.vanity_tax,
                    "low_priority_tax": receipt.low_priority_tax,
                    "blood_pact_skim": blood_pact_skim,
                    "event_win_multiplier": event_win_multiplier,
                    "event_loss_multiplier": event_loss_multiplier,
                })
            },
            reminders,
        )?;
        receipt.balance_after = tail.balance;
        receipt.net = receipt.net.saturating_sub(tail.blood_pact_skim);
        receipt.balance_delta = receipt
            .balance_after
            .saturating_sub(player.jopacoin_balance);
        return Ok(WheelSpinOutcome::Exploded(Box::new(ExplodedWheelSpin {
            receipt,
            blood_pact_skim: tail.blood_pact_skim,
            next_spin_at: tail.next_spin_at,
            bonus_spin,
            warnings: tail.warnings,
        })));
    }

    let insurance_activated = if kind == WheelKind::Bankrupt
        && effects.color.as_deref() == Some("Green")
        && matches!(landed.value, WheelValue::Numeric(value) if value < 0)
    {
        ManaRepository::new(path)
            .claim_bankrupt_buff_atomic(user_id, Some(guild_id), BankruptBuff::Insurance)
            .map_err(|error| error.to_string())?
    } else {
        false
    };
    if insurance_activated {
        landed = WheelWedge {
            label: "LOSE".to_owned(),
            value: WheelValue::Numeric(0),
            color: "#4a4a4a",
        };
    }

    // Python sends a small Blue-mana preview for the bankrupt wheel before
    // the animation.  Sampling here consumes randomness only after the
    // landed wedge is fixed, so it cannot change the settlement outcome.
    let blue_preview = (kind == WheelKind::Bankrupt && effects.color.as_deref() == Some("Blue"))
        .then(|| wheel_bankrupt_preview(&wedges));

    let reroll_available = if kind == WheelKind::Bankrupt
        && effects.color.as_deref() == Some("Red")
        && matches!(
            landed.value,
            WheelValue::Numeric(0)
                | WheelValue::Mechanic(WheelMechanic::ExtendOne | WheelMechanic::ExtendTwo)
        ) {
        !ManaRepository::new(path)
            .is_bankrupt_buff_used(user_id, Some(guild_id), BankruptBuff::Reroll)
            .map_err(|error| error.to_string())?
    } else {
        false
    };
    if let Some((key, interaction, options)) = plan_pending_wheel_interaction(
        &wedges,
        &landed,
        kind,
        &effects,
        &event_id,
        reroll_available,
    ) {
        return Ok(WheelSpinOutcome::Pending(Box::new(PendingWheelPlan {
            key,
            interaction,
            options,
            wedges,
            spin_kind: kind,
            balance_before: player.jopacoin_balance,
            effects,
            event_id,
            event_win_multiplier,
            event_loss_multiplier,
            bonus_spin,
            last_regular_spin,
            blue_preview,
        })));
    }

    let resolution = resolve_wheel_value(
        path,
        guild_id,
        user_id,
        player.jopacoin_balance,
        &landed,
        kind,
        &effects,
        config,
        &event_id,
        vanity_taxable_ids,
        low_priority_taxable_ids,
        event_win_multiplier,
        event_loss_multiplier,
        bonus_spin,
        visible_positive_balance,
        visible_member_ids,
        player_names,
        fastrand::u64(..),
        now,
    )?;
    let outcome = settle_landed_wedge(
        &LandedWheelSettlement {
            path,
            guild_id,
            user_id,
            now,
            kind,
            effects: &effects,
            config,
            event_id: &event_id,
            event_win_multiplier,
            event_loss_multiplier,
            bonus_spin,
            last_regular_spin,
            insurance_activated,
            interactive: false,
        },
        &landed,
        player.jopacoin_balance,
        resolution,
        reminders,
    )?;
    Ok(WheelSpinOutcome::Settled(Box::new(SettledWheelSpin {
        wedges,
        landed_index: index,
        outcome,
        blue_preview,
        golden_leaderboard,
    })))
}

/// Decide whether a landed wedge opens an interactive choice, returning the
/// interaction key, its kind, and the options offered.
fn plan_pending_wheel_interaction(
    wedges: &[WheelWedge],
    landed: &WheelWedge,
    kind: WheelKind,
    effects: &ManaEffects,
    event_id: &str,
    reroll_available: bool,
) -> Option<(String, WheelInteractionKind, Vec<WheelOption>)> {
    let mut options = Vec::new();
    let interaction_kind = if kind == WheelKind::Regular
        && effects.color.as_deref() == Some("Blue")
        && effects.blue_gamba_scrying
    {
        let first = fastrand::usize(..wedges.len());
        let mut second = fastrand::usize(..wedges.len());
        while second == first && wedges.len() > 1 {
            second = fastrand::usize(..wedges.len());
        }
        options.push(WheelOption::from_wedge(&wedges[first]));
        options.push(WheelOption::from_wedge(&wedges[second]));
        Some(WheelInteractionKind::Scrying)
    } else if kind == WheelKind::Bankrupt
        && matches!(landed.value, WheelValue::Mechanic(WheelMechanic::TownTrial))
    {
        options = interactive_choices(wedges);
        Some(WheelInteractionKind::TownTrial)
    } else if kind == WheelKind::Bankrupt
        && matches!(landed.value, WheelValue::Mechanic(WheelMechanic::Discover))
    {
        options = interactive_choices(wedges);
        Some(WheelInteractionKind::Discover)
    } else if kind == WheelKind::Bankrupt
        && reroll_available
        && effects.color.as_deref() == Some("Red")
        && matches!(
            landed.value,
            WheelValue::Numeric(0)
                | WheelValue::Mechanic(WheelMechanic::ExtendOne | WheelMechanic::ExtendTwo)
        )
    {
        options.push(WheelOption::from_wedge(landed));
        let candidates = wedges
            .iter()
            .filter(|wedge| wedge.value != landed.value)
            .collect::<Vec<_>>();
        let replacement = candidates
            .get(fastrand::usize(..candidates.len().max(1)))
            .copied()
            .unwrap_or(landed);
        options.push(WheelOption::from_wedge(replacement));
        Some(WheelInteractionKind::Reroll)
    } else {
        None
    }?;
    let valid = match interaction_kind {
        WheelInteractionKind::Scrying | WheelInteractionKind::Reroll => options.len() == 2,
        WheelInteractionKind::TownTrial | WheelInteractionKind::Discover => !options.is_empty(),
    };
    if !valid {
        return None;
    }
    let key = match interaction_kind {
        WheelInteractionKind::TownTrial => format!("tt_{event_id}"),
        WheelInteractionKind::Discover => format!("disc_{event_id}"),
        WheelInteractionKind::Scrying => format!("betting:scry:{event_id}"),
        WheelInteractionKind::Reroll => format!("betting:reroll:{event_id}"),
    };
    Some((key, interaction_kind, options))
}

/// Three shuffled bankrupt-wheel fates for Town Trial and Discover, never
/// another interactive wedge.
fn interactive_choices(wedges: &[WheelWedge]) -> Vec<WheelOption> {
    let mut options = wedges
        .iter()
        .filter(|wedge| {
            !matches!(
                wedge.value,
                WheelValue::Mechanic(
                    WheelMechanic::TownTrial
                        | WheelMechanic::Discover
                        | WheelMechanic::ChainReaction
                )
            )
        })
        .map(WheelOption::from_wedge)
        .collect::<Vec<_>>();
    fastrand::shuffle(&mut options);
    options.truncate(3);
    options
}

/// One settlement attempt for a pending interactive wheel.
pub struct PendingWheelRequest<'a> {
    pub path: &'a Path,
    pub guild_id: i64,
    pub user_id: i64,
    pub balance_before: i64,
    pub kind: WheelKind,
    pub effects: &'a ManaEffects,
    pub config: &'a WheelSettlementConfig,
    pub event_id: &'a str,
    pub option: &'a WheelOption,
    pub now: i64,
    pub use_reroll: bool,
    pub bonus_spin: bool,
    pub last_regular_spin: Option<i64>,
    pub event_win_multiplier: f64,
    pub event_loss_multiplier: f64,
    pub vanity_taxable_ids: &'a BTreeSet<i64>,
    pub low_priority_taxable_ids: &'a BTreeSet<i64>,
    pub visible_member_ids: Option<&'a BTreeSet<i64>>,
    pub player_names: &'a dyn WheelPlayerNameSource,
    pub reminders: &'a dyn WheelReminderPort,
}

/// Settle a pending interactive wheel with the chosen option. A retry after
/// a failed attempt resumes from `attempt` instead of re-running committed
/// effects. Runs on a blocking worker.
pub fn resolve_pending_wheel(
    request: &PendingWheelRequest<'_>,
    attempt: &Mutex<WheelAttemptMemo>,
) -> Result<WheelOutcomeSummary, String> {
    let PendingWheelRequest {
        path,
        guild_id,
        user_id,
        balance_before,
        kind,
        effects,
        config,
        event_id,
        option,
        now,
        use_reroll,
        bonus_spin,
        last_regular_spin,
        event_win_multiplier,
        event_loss_multiplier,
        vanity_taxable_ids,
        low_priority_taxable_ids,
        visible_member_ids,
        player_names,
        reminders,
    } = *request;
    let recorded = lock_wheel_attempt(attempt).clone();
    if use_reroll && !recorded.reroll_claimed {
        if !ManaRepository::new(path)
            .claim_bankrupt_buff_atomic(user_id, Some(guild_id), BankruptBuff::Reroll)
            .map_err(|error| error.to_string())?
        {
            return Err("Your Red mana re-roll has already been used today.".to_owned());
        }
        lock_wheel_attempt(attempt).reroll_claimed = true;
    }
    let landed = option.wedge();
    // A retry after a later step failed reuses the committed outcome rather
    // than re-running effects that are not keyed to this spin.
    let (outcome_balance_before, resolution) = match recorded.outcome {
        Some(outcome) => outcome,
        None => {
            let rng_seed = *lock_wheel_attempt(attempt)
                .rng_seed
                .get_or_insert_with(|| fastrand::u64(..));
            let players = PlayerRepository::new(path);
            let outcome_balance_before = players
                .get_by_id(user_id, Some(guild_id))
                .map_err(|error| error.to_string())?
                .map_or(balance_before, |row| row.jopacoin_balance);
            // Dividend pays from the same member-filtered balance total as a
            // direct spin.
            let visible_positive_balance = match visible_member_ids {
                Some(_) => visible_wheel_positive_balance(
                    &players
                        .get_leaderboard(Some(guild_id), i64::from(i32::MAX), 0)
                        .map_err(|error| error.to_string())?,
                    visible_member_ids,
                ),
                None => None,
            };
            let resolution = resolve_wheel_value(
                path,
                guild_id,
                user_id,
                outcome_balance_before,
                &landed,
                kind,
                effects,
                config,
                event_id,
                vanity_taxable_ids,
                low_priority_taxable_ids,
                event_win_multiplier,
                event_loss_multiplier,
                bonus_spin,
                visible_positive_balance,
                visible_member_ids,
                player_names,
                rng_seed,
                now,
            )?;
            lock_wheel_attempt(attempt).outcome =
                Some((outcome_balance_before, resolution.clone()));
            (outcome_balance_before, resolution)
        }
    };
    settle_landed_wedge(
        &LandedWheelSettlement {
            path,
            guild_id,
            user_id,
            now,
            kind,
            effects,
            config,
            event_id,
            event_win_multiplier,
            event_loss_multiplier,
            bonus_spin,
            last_regular_spin,
            // Insurance only replaces a direct bankrupt loss; an interactive
            // wheel never consumes it.
            insurance_activated: false,
            interactive: true,
        },
        &landed,
        outcome_balance_before,
        resolution,
        reminders,
    )
}

#[cfg(test)]
#[path = "wheel_settlement/tests.rs"]
mod tests;
