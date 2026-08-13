//! Existing-schema SQLite composition for the daily economy-event controller.
//!
//! The generic policy service deliberately models an abstract two-phase event
//! store. Production SQLite already has a stronger primitive: one transaction
//! inserts the unique guild-day row, applies every direct balance effect, and
//! records its ledger context. This facade reuses the policy math while routing
//! activation and delivery stamps through that atomic repository operation.

use std::path::Path;
use std::sync::Mutex;

use cama_db::economy_event_repository as db;
use thiserror::Error;

use crate::economy_event_service::{
    BalanceSheet, Clock, DeterministicEventRng, Direction, EVENT_CATALOG, EconomyEventConfig,
    EconomyEventEffects, EconomyEventError, EconomyEventService, EventRng, FixedClock, GuildId,
    MemoryEconomy, MemoryEventStore, SECOND_ANNOUNCEMENT_DELAY_SECONDS, SurfaceVolumes,
    SystemClock,
};

type PlanningService =
    EconomyEventService<MemoryEventStore, MemoryEconomy, FixedClock, DeterministicEventRng>;

#[derive(Debug, Error)]
pub enum SqliteEconomyEventError {
    #[error(transparent)]
    Repository(#[from] db::EconomyEventRepositoryError),
    #[error(transparent)]
    Policy(#[from] EconomyEventError),
    #[error("economy-event RNG lock is poisoned")]
    RngLock,
    #[error("economy-event catalog has no {0} candidates")]
    EmptyCatalog(&'static str),
}

/// Production controller over the schema admitted by `cama-runtime`.
///
/// Methods are synchronous by design. Runtime callers must execute them on
/// Tokio's blocking pool because every operation may open SQLite connections.
pub struct SqliteEconomyEventService<C = SystemClock, R = DeterministicEventRng> {
    repository: db::EconomyEventRepository,
    config: EconomyEventConfig,
    clock: C,
    rng: Mutex<R>,
}

impl SqliteEconomyEventService<SystemClock, DeterministicEventRng> {
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>, config: EconomyEventConfig) -> Self {
        Self::with_clock_and_rng(database_path, config, SystemClock, DeterministicEventRng)
    }
}

impl<C, R> SqliteEconomyEventService<C, R>
where
    C: Clock,
    R: EventRng,
{
    #[must_use]
    pub fn with_clock_and_rng(
        database_path: impl AsRef<Path>,
        config: EconomyEventConfig,
        clock: C,
        rng: R,
    ) -> Self {
        Self {
            repository: db::EconomyEventRepository::new(database_path),
            config: config.normalized(),
            clock,
            rng: Mutex::new(rng),
        }
    }

    #[must_use]
    pub const fn config(&self) -> &EconomyEventConfig {
        &self.config
    }

    pub fn ensure_daily_event(
        &self,
        guild_id: i64,
    ) -> Result<(Option<db::EconomyEvent>, bool), SqliteEconomyEventError> {
        self.ensure_daily_event_at(guild_id, self.clock.now())
    }

    /// Create and atomically apply the active guild-day event, or recover the
    /// already committed row after a process/Discord failure.
    pub fn ensure_daily_event_at(
        &self,
        guild_id: i64,
        now: i64,
    ) -> Result<(Option<db::EconomyEvent>, bool), SqliteEconomyEventError> {
        let planner = self.planning_service(now);
        let event_date = planner.event_date_for_timestamp(now)?;
        let policy_mode = if self.config.enabled {
            db::PolicyMode::Normal
        } else {
            db::PolicyMode::Disabled
        };
        let policy = self.repository.ensure_policy_state(
            Some(guild_id),
            policy_mode,
            self.config.normal_annual_rate,
            self.config.inflation_ceiling,
            now,
        )?;
        if !self.config.enabled || policy.mode == db::PolicyMode::Disabled {
            return Ok((None, false));
        }
        if let Some(existing) = self
            .repository
            .get_event_for_date(Some(guild_id), &event_date)?
        {
            self.ensure_activation_snapshot(guild_id, &event_date, now)?;
            return Ok((Some(existing), false));
        }
        if planner.is_before_daily_trigger(now)? {
            return Ok((None, false));
        }

        let before = self.repository.capture_balance_sheet(Some(guild_id))?;
        self.repository.reconcile_prior_event(
            Some(guild_id),
            &event_date,
            before.monetary_stock,
        )?;
        let trends =
            self.repository
                .get_monetary_trends(Some(guild_id), before.monetary_stock, now)?;
        let (direction, severity, required_effect, observed_daily_flow) = trends
            .get(&7)
            .and_then(Option::as_ref)
            .map_or((Direction::Neutral, 1, 0, 0), |weekly| {
                let elapsed_days = weekly.elapsed_days.max(1.0);
                let target_period_rate =
                    (1.0 + policy.target_annual_rate).powf(elapsed_days / 365.0) - 1.0;
                let weekly_deviation = weekly.period_rate - target_period_rate;
                let (direction, severity) =
                    PlanningService::band_for_weekly_deviation(weekly_deviation);
                let target_change =
                    (weekly.anchor_stock as f64 * target_period_rate).round_ties_even() as i64;
                let required_effect = ((target_change - weekly.change_jc) as f64 / elapsed_days)
                    .round_ties_even() as i64;
                let observed_daily_flow =
                    (weekly.change_jc as f64 / elapsed_days).round_ties_even() as i64;
                (direction, severity, required_effect, observed_daily_flow)
            });
        let db_volumes = self.repository.get_surface_daily_volumes(
            Some(guild_id),
            self.config.lookback_days,
            now,
        )?;
        let balance_sheet = app_balance_sheet(&before);
        let volumes = app_surface_volumes(&db_volumes);
        let mut candidates = EVENT_CATALOG
            .iter()
            .copied()
            .filter(|template| template.direction == direction)
            .map(|template| {
                let effects = planner.effects_for(template, severity, &balance_sheet);
                let expected = PlanningService::estimate_effect(&effects, &volumes, &balance_sheet);
                (
                    required_effect.abs_diff(expected),
                    template,
                    effects,
                    expected,
                )
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| candidate.0);
        let shortlist_len = candidates.len().min(3);
        if shortlist_len == 0 {
            return Err(SqliteEconomyEventError::EmptyCatalog(direction.as_str()));
        }
        let selected = self
            .rng
            .lock()
            .map_err(|_| SqliteEconomyEventError::RngLock)?
            .choose_index(GuildId(guild_id), &event_date, shortlist_len);
        let (_, template, effects, expected_effect_jc) =
            candidates[selected.min(shortlist_len - 1)].clone();
        let (starts_at, ends_at) = planner.event_window(&event_date)?;
        let draft = db::EventDraft {
            event_date: event_date.clone(),
            name: template.name.to_owned(),
            hero: template.hero.to_owned(),
            direction: db_direction(direction),
            severity: i64::from(severity),
            target_effect_jc: required_effect,
            forecast_flow_jc: observed_daily_flow,
            expected_effect_jc,
            monetary_stock_before: before.monetary_stock,
            effects: db_effects(&effects),
            announcement: PlanningService::announcement_text(
                template,
                severity,
                &effects,
                required_effect,
                observed_daily_flow,
            ),
            starts_at,
            ends_at,
            created_at: now,
        };
        let activated = self
            .repository
            .activate_event_atomic(Some(guild_id), &draft)?;
        self.ensure_activation_snapshot(guild_id, &event_date, now)?;
        Ok((Some(activated.event), activated.created))
    }

    pub fn seconds_until_next_trigger(&self) -> Result<i64, SqliteEconomyEventError> {
        self.seconds_until_next_trigger_at(self.clock.now())
    }

    pub fn seconds_until_next_trigger_at(&self, now: i64) -> Result<i64, SqliteEconomyEventError> {
        Ok(self
            .planning_service(now)
            .seconds_until_next_trigger(Some(now))?)
    }

    /// Apply the same active Pacific-day reward multiplier used by Python.
    /// This is a read-only operation; command providers call it after central
    /// minigame scaling and fall back to that scaled amount on error.
    pub fn adjust_reward_at(
        &self,
        guild_id: i64,
        amount: i64,
        now: i64,
    ) -> Result<i64, SqliteEconomyEventError> {
        if amount <= 0 || !self.config.enabled {
            return Ok(amount);
        }
        let multiplier = self.effects_at(guild_id, now)?.reward_multiplier;
        Ok((((amount as f64 * multiplier) + 0.5) as i64).max(0))
    }

    /// Read the active Pacific-day effects without creating an event or
    /// mutating its delivery state. Match and prediction settlement use this
    /// so their disclosed multipliers share the facade's DST/date policy.
    pub fn effects_at(
        &self,
        guild_id: i64,
        now: i64,
    ) -> Result<EconomyEventEffects, SqliteEconomyEventError> {
        if !self.config.enabled {
            return Ok(EconomyEventEffects::default());
        }
        let event_date = self.planning_service(now).event_date_for_timestamp(now)?;
        Ok(self
            .repository
            .get_event_for_date(Some(guild_id), &event_date)?
            .filter(|event| event.starts_at <= now && now < event.ends_at)
            .map_or_else(EconomyEventEffects::default, |event| {
                app_event_effects(&event.effects)
            }))
    }

    #[must_use]
    pub fn pending_announcement_slot(event: &db::EconomyEvent, now: i64) -> Option<&'static str> {
        if event.announced_at.is_none() {
            return Some("initial");
        }
        if event.reminder_announced_at.is_some() {
            return None;
        }
        let reminder_at = event.starts_at + SECOND_ANNOUNCEMENT_DELAY_SECONDS;
        (reminder_at <= now && now < event.ends_at).then_some("reminder")
    }

    pub fn mark_event_announced(
        &self,
        guild_id: i64,
        event_id: i64,
    ) -> Result<bool, SqliteEconomyEventError> {
        Ok(self
            .repository
            .mark_event_announced(Some(guild_id), event_id, self.clock.now())?)
    }

    pub fn mark_event_announced_at(
        &self,
        guild_id: i64,
        event_id: i64,
        now: i64,
    ) -> Result<bool, SqliteEconomyEventError> {
        Ok(self
            .repository
            .mark_event_announced(Some(guild_id), event_id, now)?)
    }

    pub fn mark_event_reminder_announced(
        &self,
        guild_id: i64,
        event_id: i64,
    ) -> Result<bool, SqliteEconomyEventError> {
        Ok(self.repository.mark_event_reminder_announced(
            Some(guild_id),
            event_id,
            self.clock.now(),
        )?)
    }

    pub fn mark_event_reminder_announced_at(
        &self,
        guild_id: i64,
        event_id: i64,
        now: i64,
    ) -> Result<bool, SqliteEconomyEventError> {
        Ok(self
            .repository
            .mark_event_reminder_announced(Some(guild_id), event_id, now)?)
    }

    fn planning_service(&self, now: i64) -> PlanningService {
        EconomyEventService::new(
            MemoryEventStore::default(),
            MemoryEconomy::default(),
            FixedClock::new(now),
            DeterministicEventRng,
            self.config.clone(),
        )
    }

    fn ensure_activation_snapshot(
        &self,
        guild_id: i64,
        event_date: &str,
        now: i64,
    ) -> Result<(), SqliteEconomyEventError> {
        let already_captured = self
            .repository
            .get_latest_snapshot(Some(guild_id))?
            .is_some_and(|snapshot| snapshot.snapshot_date == event_date);
        if !already_captured {
            let after = self.repository.capture_balance_sheet(Some(guild_id))?;
            self.repository
                .save_snapshot(Some(guild_id), event_date, &after, now)?;
        }
        Ok(())
    }
}

fn app_balance_sheet(sheet: &db::BalanceSheet) -> BalanceSheet {
    BalanceSheet {
        player_wallets: sheet.player_wallets,
        positive_wallets: sheet.positive_wallets,
        visible_debt: sheet.visible_debt,
        player_count: sheet.player_count,
        average_wallet: sheet.average_wallet,
        reserve_available: sheet.reserve_available,
        reserve_locked: sheet.reserve_locked,
        reserve_next_match_pot: sheet.reserve_next_match_pot,
        prediction_open_cash: sheet.prediction_open_cash,
        wager_escrow: sheet.wager_escrow,
        monetary_stock: sheet.monetary_stock,
    }
}

fn app_surface_volumes(volumes: &db::SurfaceVolumes) -> SurfaceVolumes {
    SurfaceVolumes {
        reward_credits: volumes.reward_credits,
        gamba_credits: volumes.gamba_credits,
        gamba_debits: volumes.gamba_debits,
        bet_payouts: volumes.bet_payouts,
        prediction_payouts: volumes.prediction_payouts,
    }
}

fn app_event_effects(effects: &db::EventEffects) -> EconomyEventEffects {
    EconomyEventEffects {
        reward_multiplier: effects.reward_multiplier,
        gamba_win_multiplier: effects.gamba_win_multiplier,
        gamba_loss_multiplier: effects.gamba_loss_multiplier,
        bet_payout_multiplier: effects.bet_payout_multiplier,
        prediction_payout_multiplier: effects.prediction_payout_multiplier,
        prediction_depth_multiplier: effects.prediction_depth_multiplier,
        prediction_spread_ticks_delta: i32::try_from(effects.prediction_spread_ticks_delta)
            .unwrap_or_else(|_| {
                if effects.prediction_spread_ticks_delta.is_negative() {
                    i32::MIN
                } else {
                    i32::MAX
                }
            }),
        reserve_burn_jc: effects.reserve_burn_jc,
        reserve_release_jc: effects.reserve_release_jc,
        wallet_burn_rate: effects.wallet_burn_rate,
        wallet_burn_jc: effects.wallet_burn_jc,
        reserve_voting_disabled: effects.reserve_voting_disabled,
    }
}

const fn db_direction(direction: Direction) -> db::EventDirection {
    match direction {
        Direction::Deflationary => db::EventDirection::Deflationary,
        Direction::Neutral => db::EventDirection::Neutral,
        Direction::Boon => db::EventDirection::Boon,
    }
}

fn db_effects(effects: &crate::economy_event_service::EconomyEventEffects) -> db::EventEffects {
    db::EventEffects {
        reward_multiplier: effects.reward_multiplier,
        gamba_win_multiplier: effects.gamba_win_multiplier,
        gamba_loss_multiplier: effects.gamba_loss_multiplier,
        bet_payout_multiplier: effects.bet_payout_multiplier,
        prediction_payout_multiplier: effects.prediction_payout_multiplier,
        prediction_depth_multiplier: effects.prediction_depth_multiplier,
        prediction_spread_ticks_delta: i64::from(effects.prediction_spread_ticks_delta),
        reserve_burn_jc: effects.reserve_burn_jc,
        reserve_release_jc: effects.reserve_release_jc,
        wallet_burn_rate: effects.wallet_burn_rate,
        wallet_burn_jc: effects.wallet_burn_jc,
        reserve_voting_disabled: effects.reserve_voting_disabled,
    }
}

#[cfg(test)]
mod tests {
    use cama_db::economy_event_repository::{
        EconomyEventRepository, EventDirection, EventDraft, EventEffects,
    };
    use cama_db::schema_manager::initialize_or_migrate;
    use tempfile::NamedTempFile;

    use super::{EconomyEventConfig, SqliteEconomyEventService};

    const GUILD: i64 = 8_871;
    const NOW: i64 = 1_800_000_000;

    fn service(
        database: &NamedTempFile,
    ) -> SqliteEconomyEventService<
        crate::economy_event_service::FixedClock,
        crate::economy_event_service::DeterministicEventRng,
    > {
        initialize_or_migrate(database.path()).expect("migrate database");
        SqliteEconomyEventService::with_clock_and_rng(
            database.path(),
            EconomyEventConfig::default(),
            crate::economy_event_service::FixedClock::new(NOW),
            crate::economy_event_service::DeterministicEventRng,
        )
    }

    #[test]
    fn effects_at_returns_neutral_effects_when_the_pacific_day_has_no_event() {
        let database = NamedTempFile::new().expect("temporary database");
        let service = service(&database);

        assert_eq!(
            service.effects_at(GUILD, NOW).expect("read effects"),
            crate::economy_event_service::EconomyEventEffects::default()
        );
    }

    #[test]
    fn effects_at_returns_the_persisted_non_default_active_multiplier() {
        let database = NamedTempFile::new().expect("temporary database");
        let service = service(&database);
        let event_date = service
            .planning_service(NOW)
            .event_date_for_timestamp(NOW)
            .expect("Pacific event date");
        let effects = EventEffects {
            reward_multiplier: 1.15,
            bet_payout_multiplier: 0.73,
            ..EventEffects::default()
        };
        EconomyEventRepository::new(database.path())
            .activate_event_atomic(
                Some(GUILD),
                &EventDraft {
                    event_date,
                    name: "Focused event".to_owned(),
                    hero: "Axe".to_owned(),
                    direction: EventDirection::Neutral,
                    severity: 1,
                    target_effect_jc: 0,
                    forecast_flow_jc: 0,
                    expected_effect_jc: 0,
                    monetary_stock_before: 0,
                    effects,
                    announcement: "Focused event".to_owned(),
                    starts_at: NOW - 60,
                    ends_at: NOW + 60,
                    created_at: NOW - 60,
                },
            )
            .expect("persist active event");

        let active = service.effects_at(GUILD, NOW).expect("read effects");
        assert_eq!(active.bet_payout_multiplier, 0.73);
        assert_eq!(active.reward_multiplier, 1.15);
    }
}
