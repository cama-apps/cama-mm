use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use super::*;

const GUILD: i64 = 4242;

#[derive(Default)]
struct FakePort {
    effects: Mutex<Option<BetManaEffects>>,
    mana_calls: AtomicUsize,
    placed: Mutex<Vec<(i64, &'static str, i64, i64)>>,
    adjusted: Mutex<Vec<(Option<i64>, i64)>>,
    daily_result: Mutex<Option<i64>>,
    credits: Mutex<Vec<SteadyBonusCredit>>,
    confirmations: Mutex<Vec<(String, bool)>>,
    refresh_calls: AtomicUsize,
    refresh_fails: bool,
    overlap: Option<Arc<Barrier>>,
}

impl BetCommandPort for FakePort {
    fn mana_effects(
        &self,
        _user_id: DiscordId,
        _guild_id: Option<GuildId>,
    ) -> Result<Option<BetManaEffects>, String> {
        self.mana_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.effects.lock().expect("effects").clone())
    }

    fn place_bet(&self, request: &BetCommandRequest<'_>) -> Result<(), String> {
        self.placed.lock().expect("placed").push((
            request.user_id,
            team_key(request.team),
            request.amount,
            request.leverage,
        ));
        Ok(())
    }

    fn adjust_daily_reward(&self, guild_id: Option<GuildId>, amount: i64) -> Result<i64, String> {
        self.adjusted
            .lock()
            .expect("adjusted")
            .push((guild_id, amount));
        Ok(self
            .daily_result
            .lock()
            .expect("daily result")
            .unwrap_or(amount))
    }

    fn credit_steady_bonus(&self, credit: &SteadyBonusCredit) -> Result<(), String> {
        self.credits.lock().expect("credits").push(credit.clone());
        Ok(())
    }

    fn refresh_wagers(
        &self,
        _guild_id: Option<GuildId>,
        _pending: &PendingBetState,
    ) -> Result<(), String> {
        self.refresh_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(overlap) = &self.overlap {
            overlap.wait();
        }
        if self.refresh_fails {
            Err("display unavailable".to_owned())
        } else {
            Ok(())
        }
    }

    fn send_confirmation(&self, content: &str, ephemeral: bool) -> Result<(), String> {
        self.confirmations
            .lock()
            .expect("confirmations")
            .push((content.to_owned(), ephemeral));
        if let Some(overlap) = &self.overlap {
            overlap.wait();
        }
        Ok(())
    }
}

fn request<'a>(pending: &'a PendingBetState, leverage: i64) -> BetCommandRequest<'a> {
    BetCommandRequest {
        guild_id: Some(GUILD),
        user_id: 987_654,
        team: Team::Radiant,
        amount: 5,
        leverage,
        pending,
        minigame_scale_basis_points: 10_000,
    }
}

#[test]
fn test_green_bet_reward_scales_before_daily_event() {
    let pending = PendingBetState {
        pending_match_id: 77,
        betting_mode: BettingMode::House,
    };
    let port = FakePort::default();
    *port.effects.lock().expect("effects") = Some(BetManaEffects {
        match_bet_steady_bonus: 1,
        badge: Some("🌲".to_owned()),
        ..BetManaEffects::default()
    });
    *port.daily_result.lock().expect("daily result") = Some(2);

    let outcome = execute_bet_command(&port, request(&pending, 1)).expect("bet command");

    assert_eq!(
        port.adjusted.lock().expect("adjusted").as_slice(),
        &[(Some(GUILD), 1)]
    );
    let credits = port.credits.lock().expect("credits");
    assert_eq!(credits.len(), 1);
    assert_eq!(
        credits[0],
        SteadyBonusCredit {
            user_id: 987_654,
            guild_id: Some(GUILD),
            amount: 2,
            source: "mana_reward",
            actor_id: 987_654,
            related_type: "match_bet_steady_bonus",
            related_id: 77,
            reason: "scaled Green mana bet-placement reward",
            base_reward: 1,
            adjusted_reward: 2,
        }
    );
    assert!(outcome.confirmation.starts_with("🌲 Bet placed"));
    assert_eq!(port.refresh_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn test_red_10x_bet_reuses_mana_effects_for_gate_and_badge() {
    let pending = PendingBetState {
        pending_match_id: 78,
        betting_mode: BettingMode::House,
    };
    let port = FakePort::default();
    *port.effects.lock().expect("effects") = Some(BetManaEffects {
        red_10x_leverage: true,
        badge: Some("⛰️".to_owned()),
        ..BetManaEffects::default()
    });

    let mut command = request(&pending, 10);
    command.user_id = 987_655;
    command.team = Team::Dire;
    let outcome = execute_bet_command(&port, command).expect("red 10x bet");

    assert_eq!(port.mana_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        port.placed.lock().expect("placed").as_slice(),
        &[(987_655, "dire", 5, 10)]
    );
    assert!(
        outcome
            .confirmation
            .starts_with("⛰️ Bet placed (Match #78)")
    );
    assert!(
        outcome
            .confirmation
            .contains("at 10x leverage (effective: 50")
    );
    assert_eq!(outcome.mana_resolutions, 1);
}

#[test]
fn test_bet_confirmation_overlaps_wager_refresh() {
    let pending = PendingBetState {
        pending_match_id: 79,
        betting_mode: BettingMode::House,
    };
    let port = FakePort {
        overlap: Some(Arc::new(Barrier::new(2))),
        ..FakePort::default()
    };

    let outcome = execute_bet_command(&port, request(&pending, 1)).expect("overlapping bet");

    assert_eq!(port.refresh_calls.load(Ordering::SeqCst), 1);
    let confirmations = port.confirmations.lock().expect("confirmations");
    assert_eq!(confirmations.len(), 1);
    assert!(confirmations[0].0.starts_with("Bet placed (Match #79): 5 "));
    assert!(confirmations[0].0.ends_with(" on Radiant."));
    assert!(confirmations[0].1);
    assert!(!outcome.refresh_failed);
}

#[test]
fn test_bet_confirmation_survives_wager_refresh_failure() {
    let pending = PendingBetState {
        pending_match_id: 80,
        betting_mode: BettingMode::House,
    };
    let port = FakePort {
        refresh_fails: true,
        ..FakePort::default()
    };

    let outcome = execute_bet_command(&port, request(&pending, 1)).expect("committed bet");

    assert!(outcome.refresh_failed);
    let confirmations = port.confirmations.lock().expect("confirmations");
    assert_eq!(confirmations.len(), 1);
    assert!(confirmations[0].0.starts_with("Bet placed (Match #80)"));
}

#[test]
fn test_shuffle_betting_mode_validation() {
    assert_eq!(validate_betting_mode("pool"), Ok(BettingMode::Pool));
    assert_eq!(validate_betting_mode("house"), Ok(BettingMode::House));
    assert_eq!(validate_betting_mode("invalid"), Err(InvalidBettingMode));
}
