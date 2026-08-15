use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use cama_db::dig_carry_wager::{
    CARRY_BOUNDARIES, CarryProgressEntry, CarryProgressMutation, CarrySettlementOutcome,
    CarryTunnelSnapshot, ClearCarryOutcome, PersistedWager,
};

use super::*;

const ACTOR: i64 = 20_001;
const GUILD: i64 = 12_345;

#[derive(Clone, Debug)]
struct MemoryPort {
    state: Arc<Mutex<MemoryState>>,
}

#[derive(Clone, Debug)]
struct MemoryState {
    snapshot: Option<CarryTunnelSnapshot>,
    clear_count: usize,
    settlement_count: usize,
}

impl MemoryPort {
    fn new(snapshot: Option<CarryTunnelSnapshot>) -> Self {
        Self {
            state: Arc::new(Mutex::new(MemoryState {
                snapshot,
                clear_count: 0,
                settlement_count: 0,
            })),
        }
    }

    fn state(&self) -> MemoryState {
        self.state.lock().expect("memory port lock").clone()
    }
}

impl DigCarryWagerPort for MemoryPort {
    type Error = Infallible;

    fn snapshot(&self, key: CarryTunnelKey) -> Result<Option<CarryTunnelSnapshot>, Self::Error> {
        Ok(self
            .state
            .lock()
            .expect("memory port lock")
            .snapshot
            .clone()
            .filter(|snapshot| snapshot.key == key))
    }

    fn clear_carry_markers(
        &self,
        expected: &CarryTunnelSnapshot,
        boundary: i64,
    ) -> Result<ClearCarryOutcome, Self::Error> {
        let mut state = self.state.lock().expect("memory port lock");
        let Some(snapshot) = state.snapshot.as_mut() else {
            return Ok(ClearCarryOutcome::MissingTunnel);
        };
        if !same_guard(snapshot, expected) {
            return Ok(ClearCarryOutcome::Conflict);
        }
        let entry = snapshot.entry_mut(boundary);
        entry.has_wager_marker = false;
        entry.wager = PersistedWager::Missing;
        entry.has_risk_marker = false;
        entry.risk_tier = None;
        snapshot.boss_progress.push('#');
        let boss_progress = snapshot.boss_progress.clone();
        state.clear_count += 1;
        Ok(ClearCarryOutcome::Cleared { boss_progress })
    }

    fn settle_atomic(
        &self,
        request: AtomicCarrySettlement<'_>,
    ) -> Result<CarrySettlementOutcome, Self::Error> {
        let mut state = self.state.lock().expect("memory port lock");
        let Some(snapshot) = state.snapshot.as_mut() else {
            return Ok(CarrySettlementOutcome::MissingTunnel);
        };
        if snapshot.key != request.key
            || snapshot.depth != request.expected_depth
            || snapshot.pinnacle_phase != request.expected_pinnacle_phase
            || snapshot.boss_progress != request.expected_boss_progress
        {
            return Ok(CarrySettlementOutcome::Conflict);
        }
        let Some(balance_before) = snapshot.balance else {
            return Ok(CarrySettlementOutcome::MissingPlayer);
        };
        let actual_balance_delta = if request.requested_balance_delta < 0 {
            let debit = request
                .requested_balance_delta
                .unsigned_abs()
                .min(balance_before.max(0) as u64) as i64;
            -debit
        } else {
            request.requested_balance_delta
        };
        let balance_after = balance_before + actual_balance_delta;
        let boundary = match request.mutation {
            CarryProgressMutation::Clear { boundary, .. }
            | CarryProgressMutation::Forward { boundary, .. } => boundary,
        };
        let entry = snapshot.entry_mut(boundary);
        match request.mutation {
            CarryProgressMutation::Clear { status_after, .. } => {
                entry.has_wager_marker = false;
                entry.wager = PersistedWager::Missing;
                entry.has_risk_marker = false;
                entry.risk_tier = None;
                if let Some(status_after) = status_after {
                    entry.status = Some(status_after.to_owned());
                }
            }
            CarryProgressMutation::Forward {
                status_after,
                wager,
                risk_tier,
                ..
            } => {
                entry.status = Some(status_after.to_owned());
                entry.has_wager_marker = true;
                entry.wager = PersistedWager::Integer(wager);
                entry.has_risk_marker = true;
                entry.risk_tier = Some(risk_tier.to_owned());
            }
        }
        snapshot.depth = request.depth_after;
        snapshot.pinnacle_phase = request.pinnacle_phase_after;
        snapshot.balance = Some(balance_after);
        snapshot.boss_progress.push('#');
        let boss_progress = snapshot.boss_progress.clone();
        state.settlement_count += 1;
        Ok(CarrySettlementOutcome::Applied {
            boss_progress,
            balance_before,
            balance_after,
            actual_balance_delta,
        })
    }
}

trait SnapshotEntryMut {
    fn entry_mut(&mut self, boundary: i64) -> &mut CarryProgressEntry;
}

impl SnapshotEntryMut for CarryTunnelSnapshot {
    fn entry_mut(&mut self, boundary: i64) -> &mut CarryProgressEntry {
        self.entries
            .iter_mut()
            .find_map(|(candidate, entry)| (*candidate == boundary).then_some(entry))
            .expect("fixture boundary")
    }
}

fn same_guard(actual: &CarryTunnelSnapshot, expected: &CarryTunnelSnapshot) -> bool {
    actual.key == expected.key
        && actual.depth == expected.depth
        && actual.pinnacle_phase == expected.pinnacle_phase
        && actual.boss_progress == expected.boss_progress
}

#[derive(Clone, Copy, Debug)]
struct FixedEntropy {
    retreat: i64,
    knockback: i64,
}

impl DigCarryEntropy for FixedEntropy {
    fn retreat_block_loss(&mut self) -> i64 {
        self.retreat
    }

    fn pinnacle_loss_knockback(&mut self) -> i64 {
        self.knockback
    }
}

type TestService = DigCarryWagerService<MemoryPort, FixedEntropy>;

fn key() -> CarryTunnelKey {
    CarryTunnelKey {
        discord_id: ACTOR,
        guild_id: Some(GUILD),
    }
}

fn progress_entry(status: &str, wager: PersistedWager, risk: Option<&str>) -> CarryProgressEntry {
    CarryProgressEntry {
        status: Some(status.to_owned()),
        has_wager_marker: !matches!(wager, PersistedWager::Missing),
        wager,
        has_risk_marker: risk.is_some(),
        risk_tier: risk.map(str::to_owned),
    }
}

fn empty_entry(status: &str) -> CarryProgressEntry {
    progress_entry(status, PersistedWager::Missing, None)
}

fn regular_snapshot(with_carry: bool) -> CarryTunnelSnapshot {
    let entry = if with_carry {
        progress_entry(
            "phase1_defeated",
            PersistedWager::Integer(200),
            Some("bold"),
        )
    } else {
        empty_entry("active")
    };
    CarryTunnelSnapshot {
        key: key(),
        depth: 25,
        pinnacle_phase: 1,
        boss_progress: "regular-v1".to_owned(),
        balance: Some(2_000),
        entries: CARRY_BOUNDARIES
            .into_iter()
            .map(|boundary| {
                (
                    boundary,
                    if boundary == 25 {
                        entry.clone()
                    } else {
                        CarryProgressEntry {
                            status: None,
                            has_wager_marker: false,
                            wager: PersistedWager::Missing,
                            has_risk_marker: false,
                            risk_tier: None,
                        }
                    },
                )
            })
            .collect(),
    }
}

fn pinnacle_snapshot(
    status: &str,
    phase: i64,
    wager: PersistedWager,
    risk: Option<&str>,
) -> CarryTunnelSnapshot {
    CarryTunnelSnapshot {
        key: key(),
        depth: PINNACLE_BOUNDARY,
        pinnacle_phase: phase,
        boss_progress: format!("pinnacle-{phase}-{status}"),
        balance: Some(2_000),
        entries: CARRY_BOUNDARIES
            .into_iter()
            .map(|boundary| {
                if boundary == PINNACLE_BOUNDARY {
                    (boundary, progress_entry(status, wager.clone(), risk))
                } else {
                    (boundary, empty_entry("defeated"))
                }
            })
            .collect(),
    }
}

fn service(snapshot: Option<CarryTunnelSnapshot>) -> TestService {
    DigCarryWagerService::new(
        MemoryPort::new(snapshot),
        FixedEntropy {
            retreat: 3,
            knockback: 10,
        },
    )
}

fn active_carry_snapshot(wager: i64) -> CarryTunnelSnapshot {
    pinnacle_snapshot(
        "phase1_defeated",
        2,
        PersistedWager::Integer(wager),
        Some("bold"),
    )
}

fn assert_invalid_carry_rejects_cap(
    resolver: BossResolver,
    status: &str,
    phase: i64,
    wager: PersistedWager,
    risk: &str,
) {
    let service = service(Some(pinnacle_snapshot(status, phase, wager, Some(risk))));
    let before = service.port().state();
    assert_eq!(
        service
            .prepare_fight(key(), resolver, RiskTier::Cautious, 1_001)
            .expect("prepare"),
        FightPreparation::Rejected {
            error: OVER_CAP_MESSAGE
        }
    );
    let after = service.port().state();
    assert_eq!(after.snapshot, before.snapshot);
    assert_eq!(after.settlement_count, 0);
    assert_eq!(after.clear_count, 0);
}

fn finalize_input(phase: u8, won: bool, wager: i64, risk_tier: RiskTier) -> PinnacleFinalizeInput {
    PinnacleFinalizeInput {
        phase,
        won,
        wager,
        risk_tier,
        win_chance: 0.55,
        prestige_level: 0,
    }
}

#[test]
fn test_returns_none_and_clears_regular_boss_carry() {
    let service = service(Some(regular_snapshot(true)));
    assert_eq!(service.get_carried_wager(key()).expect("lookup"), None);
    let state = service.port().state();
    assert_eq!(state.clear_count, 1);
    assert!(
        !state
            .snapshot
            .expect("tunnel")
            .entry(25)
            .expect("boundary")
            .has_either_marker()
    );
}

#[test]
fn test_returns_none_without_carry() {
    let service = service(Some(regular_snapshot(false)));
    assert_eq!(service.get_carried_wager(key()).expect("lookup"), None);
    assert_eq!(service.port().state().clear_count, 0);
}

#[test]
fn test_returns_none_when_no_tunnel() {
    let service = service(None);
    assert_eq!(service.get_carried_wager(key()).expect("lookup"), None);
}

#[test]
fn test_set_carried_wager_helper_writes_both_fields() {
    let mut entry = EditableCarryEntry {
        status: Some("active".to_owned()),
        boss_id: Some("grothak".to_owned()),
        ..EditableCarryEntry::default()
    };
    set_carried_wager(&mut entry, 150, RiskTier::Reckless);
    assert_eq!(entry.carried_wager, Some(150));
    assert_eq!(entry.carried_risk_tier, Some(RiskTier::Reckless));
    assert_eq!(entry.status.as_deref(), Some("active"));
    assert_eq!(entry.boss_id.as_deref(), Some("grothak"));
}

#[test]
fn test_clear_carried_wager_drops_only_carry_fields() {
    let mut entry = EditableCarryEntry {
        status: Some("phase1_defeated".to_owned()),
        boss_id: Some("grothak".to_owned()),
        hp_max: Some(40),
        carried_wager: Some(200),
        carried_risk_tier: Some(RiskTier::Bold),
    };
    clear_carried_wager(&mut entry);
    assert_eq!(entry.carried_wager, None);
    assert_eq!(entry.carried_risk_tier, None);
    assert_eq!(entry.status.as_deref(), Some("phase1_defeated"));
    assert_eq!(entry.hp_max, Some(40));
}

#[test]
fn test_retreat_with_stale_carry_charges_nothing() {
    let mut service = service(Some(regular_snapshot(true)));
    let result = service.retreat(key()).expect("retreat");
    assert!(matches!(
        result,
        RetreatResult::Applied {
            carried_wager_forfeit: 0,
            actual_balance_delta: 0,
            ..
        }
    ));
    let persisted = service.port().state().snapshot.expect("tunnel");
    assert_eq!(persisted.balance, Some(2_000));
    assert!(!persisted.entry(25).expect("boundary").has_either_marker());
}

#[test]
fn test_retreat_without_carry_does_not_charge() {
    let mut service = service(Some(regular_snapshot(false)));
    assert!(matches!(
        service.retreat(key()).expect("retreat"),
        RetreatResult::Applied {
            carried_wager_forfeit: 0,
            actual_balance_delta: 0,
            ..
        }
    ));
    assert_eq!(
        service.port().state().snapshot.expect("tunnel").balance,
        Some(2_000)
    );
}

#[test]
fn test_existing_carried_wager_above_new_cap_can_continue_fight_boss() {
    let service = service(Some(active_carry_snapshot(1_500)));
    assert_eq!(
        service
            .prepare_fight(key(), BossResolver::FightBoss, RiskTier::Cautious, 0,)
            .expect("prepare"),
        FightPreparation::Ready(PreparedBossFight {
            resolver: BossResolver::FightBoss,
            boundary: PINNACLE_BOUNDARY,
            wager: 1_500,
            risk_tier: RiskTier::Bold,
            inherited_carry: true,
        })
    );
}

#[test]
fn test_existing_carried_wager_above_new_cap_can_continue_start_boss_duel() {
    let service = service(Some(active_carry_snapshot(1_500)));
    assert!(matches!(
        service
            .prepare_fight(key(), BossResolver::StartBossDuel, RiskTier::Cautious, 0,)
            .expect("prepare"),
        FightPreparation::Ready(PreparedBossFight {
            resolver: BossResolver::StartBossDuel,
            wager: 1_500,
            risk_tier: RiskTier::Bold,
            inherited_carry: true,
            ..
        })
    ));
}

#[test]
fn test_invalid_active_phase1_bold_fight_boss() {
    assert_invalid_carry_rejects_cap(
        BossResolver::FightBoss,
        "active",
        1,
        PersistedWager::Integer(1_500),
        "bold",
    );
}

#[test]
fn test_invalid_active_phase1_bold_start_boss_duel() {
    assert_invalid_carry_rejects_cap(
        BossResolver::StartBossDuel,
        "active",
        1,
        PersistedWager::Integer(1_500),
        "bold",
    );
}

#[test]
fn test_invalid_phase1_defeated_phase3_bold_fight_boss() {
    assert_invalid_carry_rejects_cap(
        BossResolver::FightBoss,
        "phase1_defeated",
        3,
        PersistedWager::Integer(1_500),
        "bold",
    );
}

#[test]
fn test_invalid_phase1_defeated_phase3_bold_start_boss_duel() {
    assert_invalid_carry_rejects_cap(
        BossResolver::StartBossDuel,
        "phase1_defeated",
        3,
        PersistedWager::Integer(1_500),
        "bold",
    );
}

#[test]
fn test_invalid_phase2_defeated_phase2_bold_fight_boss() {
    assert_invalid_carry_rejects_cap(
        BossResolver::FightBoss,
        "phase2_defeated",
        2,
        PersistedWager::Integer(1_500),
        "bold",
    );
}

#[test]
fn test_invalid_phase2_defeated_phase2_bold_start_boss_duel() {
    assert_invalid_carry_rejects_cap(
        BossResolver::StartBossDuel,
        "phase2_defeated",
        2,
        PersistedWager::Integer(1_500),
        "bold",
    );
}

#[test]
fn test_invalid_negative_wager_bold_fight_boss() {
    assert_invalid_carry_rejects_cap(
        BossResolver::FightBoss,
        "phase1_defeated",
        2,
        PersistedWager::Integer(-5),
        "bold",
    );
}

#[test]
fn test_invalid_negative_wager_bold_start_boss_duel() {
    assert_invalid_carry_rejects_cap(
        BossResolver::StartBossDuel,
        "phase1_defeated",
        2,
        PersistedWager::Integer(-5),
        "bold",
    );
}

#[test]
fn test_invalid_wild_risk_fight_boss() {
    assert_invalid_carry_rejects_cap(
        BossResolver::FightBoss,
        "phase1_defeated",
        2,
        PersistedWager::Integer(1_500),
        "wild",
    );
}

#[test]
fn test_invalid_wild_risk_start_boss_duel() {
    assert_invalid_carry_rejects_cap(
        BossResolver::StartBossDuel,
        "phase1_defeated",
        2,
        PersistedWager::Integer(1_500),
        "wild",
    );
}

#[test]
fn test_invalid_nonnumeric_wager_bold_fight_boss() {
    assert_invalid_carry_rejects_cap(
        BossResolver::FightBoss,
        "phase1_defeated",
        2,
        PersistedWager::Invalid,
        "bold",
    );
}

#[test]
fn test_invalid_nonnumeric_wager_bold_start_boss_duel() {
    assert_invalid_carry_rejects_cap(
        BossResolver::StartBossDuel,
        "phase1_defeated",
        2,
        PersistedWager::Invalid,
        "bold",
    );
}

#[test]
fn test_over_cap_rejection_does_not_clear_stale_regular_carry_fight_boss() {
    let service = service(Some(regular_snapshot(true)));
    let before = service.port().state();
    assert_eq!(
        service
            .prepare_fight(key(), BossResolver::FightBoss, RiskTier::Bold, 1_001)
            .expect("prepare"),
        FightPreparation::Rejected {
            error: OVER_CAP_MESSAGE
        }
    );
    assert_eq!(service.port().state().snapshot, before.snapshot);
}

#[test]
fn test_over_cap_rejection_does_not_clear_stale_regular_carry_start_boss_duel() {
    let service = service(Some(regular_snapshot(true)));
    let before = service.port().state();
    assert_eq!(
        service
            .prepare_fight(key(), BossResolver::StartBossDuel, RiskTier::Bold, 1_001,)
            .expect("prepare"),
        FightPreparation::Rejected {
            error: OVER_CAP_MESSAGE
        }
    );
    assert_eq!(service.port().state().snapshot, before.snapshot);
}

#[test]
fn test_stale_pinnacle_carry_is_hidden_from_encounter_ui() {
    let service = service(Some(pinnacle_snapshot(
        "active",
        1,
        PersistedWager::Integer(1_500),
        Some("bold"),
    )));
    assert_eq!(service.get_carried_wager(key()).expect("lookup"), None);
    assert_eq!(
        service.encounter_info(key()).expect("encounter"),
        Some(BossEncounterInfo {
            boundary: PINNACLE_BOUNDARY,
            carried_wager: None,
        })
    );
}

#[test]
fn test_pinnacle_encounter_info_includes_carried_wager() {
    let service = service(Some(active_carry_snapshot(1_500)));
    assert_eq!(
        service.encounter_info(key()).expect("encounter"),
        Some(BossEncounterInfo {
            boundary: PINNACLE_BOUNDARY,
            carried_wager: Some(1_500),
        })
    );
}

#[test]
fn test_pinnacle_retreat_with_carried_wager_debits_half() {
    let mut service = service(Some(active_carry_snapshot(200)));
    assert_eq!(
        service.retreat(key()).expect("retreat"),
        RetreatResult::Applied {
            boundary: PINNACLE_BOUNDARY,
            block_loss: 3,
            new_depth: 347,
            carried_wager_forfeit: 100,
            actual_balance_delta: -100,
        }
    );
    let snapshot = service.port().state().snapshot.expect("tunnel");
    assert_eq!(snapshot.balance, Some(1_900));
    assert!(
        !snapshot
            .entry(PINNACLE_BOUNDARY)
            .expect("pinnacle")
            .has_either_marker()
    );
}

#[test]
fn test_full_clear_wager_payout_tapers_at_high_win_chance() {
    assert_eq!(
        pinnacle_payout(200, RiskTier::Bold, 0.95, 0).expect("payout"),
        PinnaclePayout {
            scaled_base_reward: 325,
            wager_payout: 10,
            payout: 335,
        }
    );
}

#[test]
fn test_full_clear_without_a_wager_pays_only_the_base_reward() {
    assert_eq!(
        pinnacle_payout(0, RiskTier::Bold, 0.55, 0).expect("payout"),
        PinnaclePayout {
            scaled_base_reward: 325,
            wager_payout: 0,
            payout: 325,
        }
    );
}

#[test]
fn test_three_phase_full_clear_pays_wager_profit_once() {
    let mut service = service(Some(pinnacle_snapshot(
        "active",
        1,
        PersistedWager::Missing,
        None,
    )));
    let phase1 = service
        .finalize_pinnacle(key(), finalize_input(1, true, 200, RiskTier::Bold))
        .expect("phase 1");
    assert!(matches!(
        phase1,
        FinalizeResult::Applied {
            payout: 0,
            actual_balance_delta: 0,
            next_phase: 2,
            ..
        }
    ));
    assert_eq!(
        service
            .port()
            .state()
            .snapshot
            .as_ref()
            .and_then(|row| row.balance),
        Some(2_000)
    );

    let phase2 = service
        .finalize_pinnacle(key(), finalize_input(2, true, 0, RiskTier::Cautious))
        .expect("phase 2");
    assert!(matches!(
        phase2,
        FinalizeResult::Applied {
            payout: 0,
            actual_balance_delta: 0,
            next_phase: 3,
            ..
        }
    ));

    let phase3 = service
        .finalize_pinnacle(key(), finalize_input(3, true, 0, RiskTier::Cautious))
        .expect("phase 3");
    assert!(matches!(
        phase3,
        FinalizeResult::Applied {
            wager_payout: 267,
            payout: 592,
            actual_balance_delta: 592,
            next_phase: 0,
            ..
        }
    ));
    let settled = service.port().state();
    assert_eq!(
        settled.snapshot.as_ref().and_then(|row| row.balance),
        Some(2_592)
    );
    assert_eq!(settled.settlement_count, 3);

    assert_eq!(
        service
            .finalize_pinnacle(key(), finalize_input(3, true, 200, RiskTier::Bold))
            .expect("idempotent retry"),
        FinalizeResult::NotAtPinnacle
    );
    assert_eq!(
        service.port().state().snapshot.and_then(|row| row.balance),
        Some(2_592)
    );
}

#[test]
fn test_phase2_loss_debits_wager_once_after_phase1_carry() {
    let mut service = service(Some(pinnacle_snapshot(
        "active",
        1,
        PersistedWager::Missing,
        None,
    )));
    service
        .finalize_pinnacle(key(), finalize_input(1, true, 200, RiskTier::Bold))
        .expect("phase 1");
    let loss = service
        .finalize_pinnacle(key(), finalize_input(2, false, 0, RiskTier::Cautious))
        .expect("phase 2 loss");
    assert!(matches!(
        loss,
        FinalizeResult::Applied {
            phase: 2,
            won: false,
            actual_balance_delta: -200,
            ..
        }
    ));
    assert_eq!(
        service
            .port()
            .state()
            .snapshot
            .as_ref()
            .and_then(|row| row.balance),
        Some(1_800)
    );
    assert_eq!(
        service
            .finalize_pinnacle(key(), finalize_input(2, false, 200, RiskTier::Bold))
            .expect("retry"),
        FinalizeResult::NotAtPinnacle
    );
    assert_eq!(
        service.port().state().snapshot.and_then(|row| row.balance),
        Some(1_800)
    );
}

#[test]
fn test_phase3_loss_after_two_carries_debits_wager_once() {
    let mut service = service(Some(pinnacle_snapshot(
        "active",
        1,
        PersistedWager::Missing,
        None,
    )));
    service
        .finalize_pinnacle(key(), finalize_input(1, true, 150, RiskTier::Reckless))
        .expect("phase 1");
    service
        .finalize_pinnacle(key(), finalize_input(2, true, 0, RiskTier::Cautious))
        .expect("phase 2");
    let loss = service
        .finalize_pinnacle(key(), finalize_input(3, false, 0, RiskTier::Cautious))
        .expect("phase 3 loss");
    assert!(matches!(
        loss,
        FinalizeResult::Applied {
            phase: 3,
            won: false,
            actual_balance_delta: -150,
            ..
        }
    ));
    assert_eq!(
        service
            .port()
            .state()
            .snapshot
            .as_ref()
            .and_then(|row| row.balance),
        Some(1_850)
    );
    assert_eq!(
        service
            .finalize_pinnacle(key(), finalize_input(3, false, 150, RiskTier::Reckless))
            .expect("retry"),
        FinalizeResult::NotAtPinnacle
    );
    assert_eq!(
        service.port().state().snapshot.and_then(|row| row.balance),
        Some(1_850)
    );
}
