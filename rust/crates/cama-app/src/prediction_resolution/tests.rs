use std::cell::RefCell;
use std::convert::Infallible;

use super::*;

#[derive(Debug, Default)]
struct FakeRepository {
    lock_calls: RefCell<Vec<(i64, i64)>>,
}

impl PredictionResolutionPort for FakeRepository {
    type Error = Infallible;

    fn lock_expired_predictions(&self, guild_id: i64, now: i64) -> Result<Vec<i64>, Self::Error> {
        self.lock_calls.borrow_mut().push((guild_id, now));
        Ok(vec![31, 29])
    }

    fn settle_orderbook(
        &self,
        _prediction_id: i64,
        _outcome: ContractSide,
        _resolved_by: Option<i64>,
        _adjustments: &SettlementAdjustments,
    ) -> Result<Resolution, Self::Error> {
        unreachable!("expiry delegation never calls settlement")
    }

    fn rollback_orderbook(
        &self,
        _prediction_id: i64,
        _guild_id: i64,
        _levels: &[NewLevel],
        _rolled_back_by: Option<i64>,
    ) -> Result<RollbackResult, Self::Error> {
        unreachable!("expiry delegation never calls rollback")
    }
}

#[derive(Clone, Copy, Debug)]
struct FixedClock(i64);

impl PredictionResolutionClock for FixedClock {
    fn now_seconds(&self) -> i64 {
        self.0
    }
}

#[test]
fn test_check_and_lock_expired_delegates_once_without_legacy_path() {
    let service =
        PredictionResolutionService::new(FakeRepository::default(), FixedClock(2_000_000_000));

    assert_eq!(service.check_and_lock_expired(77), Ok(vec![31, 29]));
    assert_eq!(
        *service.repository.lock_calls.borrow(),
        vec![(77, 2_000_000_000)]
    );
}
