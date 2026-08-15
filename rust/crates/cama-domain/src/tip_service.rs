//! Service-layer contracts for tip history and the atomic money path.
//!
//! Python's `TipService` is deliberately thin: it forwards history and
//! aggregate operations to `TipRepository` without normalizing guild IDs or
//! changing errors. The `/tip` money path is likewise a `PlayerService`
//! delegation to `PlayerRepository::tip_atomic`; fee-rate calculation, mana
//! modifiers, command caps, and user-facing messages are command-layer policy
//! and therefore do not belong here.

/// One tip history row returned through the service boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipRecord {
    pub id: i64,
    pub sender_id: i64,
    pub recipient_id: i64,
    pub amount: i64,
    pub fee: i64,
    pub guild_id: i64,
    pub timestamp: i64,
}

/// One sender or receiver leaderboard aggregate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipLeaderboardEntry {
    pub discord_id: i64,
    pub total_amount: i64,
    pub tip_count: i64,
}

/// Per-user aggregates returned by Python's tip service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UserTipStats {
    pub total_sent: i64,
    pub tips_sent_count: i64,
    pub fees_paid: i64,
    pub total_received: i64,
    pub tips_received_count: i64,
}

/// Guild-scoped tip volume aggregates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TipVolume {
    pub total_amount: i64,
    pub total_fees: i64,
    pub total_transactions: i64,
}

/// Atomic transfer result exposed by Python's `PlayerService`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipTransferResult {
    pub amount: i64,
    pub fee: i64,
    pub tithe: i64,
    pub nonprofit_credit: i64,
    pub from_new_balance: i64,
    pub to_new_balance: i64,
}

/// Storage operations delegated by Python's `TipService`.
pub trait TipHistoryStore {
    type Error;

    fn log_tip(
        &self,
        sender_id: i64,
        recipient_id: i64,
        amount: i64,
        fee: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, Self::Error>;

    fn get_tips_by_sender(
        &self,
        sender_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TipRecord>, Self::Error>;

    fn get_tips_by_recipient(
        &self,
        recipient_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TipRecord>, Self::Error>;

    fn get_total_fees_collected(&self, guild_id: Option<i64>) -> Result<i64, Self::Error>;

    fn get_top_senders(
        &self,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TipLeaderboardEntry>, Self::Error>;

    fn get_top_receivers(
        &self,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TipLeaderboardEntry>, Self::Error>;

    fn get_user_tip_stats(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<UserTipStats, Self::Error>;

    fn get_total_tip_volume(&self, guild_id: Option<i64>) -> Result<TipVolume, Self::Error>;
}

/// Rust counterpart of Python's repository-delegating `TipService`.
#[derive(Clone, Debug)]
pub struct TipService<R> {
    tip_repo: R,
}

impl<R> TipService<R>
where
    R: TipHistoryStore,
{
    #[must_use]
    pub const fn new(tip_repo: R) -> Self {
        Self { tip_repo }
    }

    pub fn log_tip(
        &self,
        sender_id: i64,
        recipient_id: i64,
        amount: i64,
        fee: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, R::Error> {
        self.tip_repo
            .log_tip(sender_id, recipient_id, amount, fee, guild_id)
    }

    pub fn get_tips_by_sender(
        &self,
        sender_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TipRecord>, R::Error> {
        self.tip_repo.get_tips_by_sender(sender_id, guild_id, limit)
    }

    pub fn get_tips_by_recipient(
        &self,
        recipient_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TipRecord>, R::Error> {
        self.tip_repo
            .get_tips_by_recipient(recipient_id, guild_id, limit)
    }

    pub fn get_total_fees_collected(&self, guild_id: Option<i64>) -> Result<i64, R::Error> {
        self.tip_repo.get_total_fees_collected(guild_id)
    }

    pub fn get_top_senders(
        &self,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TipLeaderboardEntry>, R::Error> {
        self.tip_repo.get_top_senders(guild_id, limit)
    }

    pub fn get_top_receivers(
        &self,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TipLeaderboardEntry>, R::Error> {
        self.tip_repo.get_top_receivers(guild_id, limit)
    }

    pub fn get_user_tip_stats(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<UserTipStats, R::Error> {
        self.tip_repo.get_user_tip_stats(discord_id, guild_id)
    }

    pub fn get_total_tip_volume(&self, guild_id: Option<i64>) -> Result<TipVolume, R::Error> {
        self.tip_repo.get_total_tip_volume(guild_id)
    }

    /// Recover the injected repository for runtime composition or tests.
    pub fn into_inner(self) -> R {
        self.tip_repo
    }
}

/// Atomic tip operation delegated by Python's `PlayerService`.
pub trait PlayerTipStore {
    type Error;

    fn tip_atomic(
        &self,
        from_discord_id: i64,
        to_discord_id: i64,
        guild_id: i64,
        amount: i64,
        fee: i64,
        tithe: i64,
    ) -> Result<TipTransferResult, Self::Error>;
}

/// Narrow Rust service for the tip portion of Python's larger `PlayerService`.
#[derive(Clone, Debug)]
pub struct PlayerTipService<P> {
    player_repo: P,
}

impl<P> PlayerTipService<P>
where
    P: PlayerTipStore,
{
    #[must_use]
    pub const fn new(player_repo: P) -> Self {
        Self { player_repo }
    }

    /// Delegate the ordinary tip path with Python's default tithe of zero.
    pub fn tip_atomic(
        &self,
        from_discord_id: i64,
        to_discord_id: i64,
        guild_id: i64,
        amount: i64,
        fee: i64,
    ) -> Result<TipTransferResult, P::Error> {
        self.tip_atomic_with_tithe(from_discord_id, to_discord_id, guild_id, amount, fee, 0)
    }

    /// Delegate the mana-modified path with the caller's already-computed tithe.
    pub fn tip_atomic_with_tithe(
        &self,
        from_discord_id: i64,
        to_discord_id: i64,
        guild_id: i64,
        amount: i64,
        fee: i64,
        tithe: i64,
    ) -> Result<TipTransferResult, P::Error> {
        self.player_repo
            .tip_atomic(from_discord_id, to_discord_id, guild_id, amount, fee, tithe)
    }

    /// Recover the injected repository for runtime composition or tests.
    pub fn into_inner(self) -> P {
        self.player_repo
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::convert::Infallible;

    use super::{
        PlayerTipService, PlayerTipStore, TipHistoryStore, TipLeaderboardEntry, TipRecord,
        TipService, TipTransferResult, TipVolume, UserTipStats,
    };

    const TEST_GUILD_ID: i64 = 123;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct LogCall {
        sender_id: i64,
        recipient_id: i64,
        amount: i64,
        fee: i64,
        guild_id: Option<i64>,
    }

    #[derive(Default)]
    struct MockTipState {
        next_id: i64,
        records: Vec<TipRecord>,
        log_calls: Vec<LogCall>,
        fee_query_calls: Vec<Option<i64>>,
    }

    #[derive(Default)]
    struct MockTipRepository {
        state: RefCell<MockTipState>,
    }

    impl TipHistoryStore for MockTipRepository {
        type Error = Infallible;

        fn log_tip(
            &self,
            sender_id: i64,
            recipient_id: i64,
            amount: i64,
            fee: i64,
            guild_id: Option<i64>,
        ) -> Result<i64, Self::Error> {
            let mut state = self.state.borrow_mut();
            state.next_id += 1;
            let id = state.next_id;
            state.log_calls.push(LogCall {
                sender_id,
                recipient_id,
                amount,
                fee,
                guild_id,
            });
            state.records.push(TipRecord {
                id,
                sender_id,
                recipient_id,
                amount,
                fee,
                guild_id: guild_id.unwrap_or(0),
                timestamp: 1_700_000_000 + id,
            });
            Ok(id)
        }

        fn get_tips_by_sender(
            &self,
            sender_id: i64,
            guild_id: Option<i64>,
            limit: i64,
        ) -> Result<Vec<TipRecord>, Self::Error> {
            let normalized_guild = guild_id.unwrap_or(0);
            let limit = usize::try_from(limit).unwrap_or(usize::MAX);
            Ok(self
                .state
                .borrow()
                .records
                .iter()
                .filter(|record| {
                    record.sender_id == sender_id && record.guild_id == normalized_guild
                })
                .take(limit)
                .cloned()
                .collect())
        }

        fn get_tips_by_recipient(
            &self,
            recipient_id: i64,
            guild_id: Option<i64>,
            limit: i64,
        ) -> Result<Vec<TipRecord>, Self::Error> {
            let normalized_guild = guild_id.unwrap_or(0);
            let limit = usize::try_from(limit).unwrap_or(usize::MAX);
            Ok(self
                .state
                .borrow()
                .records
                .iter()
                .filter(|record| {
                    record.recipient_id == recipient_id && record.guild_id == normalized_guild
                })
                .take(limit)
                .cloned()
                .collect())
        }

        fn get_total_fees_collected(&self, guild_id: Option<i64>) -> Result<i64, Self::Error> {
            let mut state = self.state.borrow_mut();
            state.fee_query_calls.push(guild_id);
            Ok(state
                .records
                .iter()
                .filter(|record| guild_id.is_none_or(|guild_id| record.guild_id == guild_id))
                .map(|record| record.fee)
                .sum())
        }

        fn get_top_senders(
            &self,
            _guild_id: Option<i64>,
            _limit: i64,
        ) -> Result<Vec<TipLeaderboardEntry>, Self::Error> {
            Ok(Vec::new())
        }

        fn get_top_receivers(
            &self,
            _guild_id: Option<i64>,
            _limit: i64,
        ) -> Result<Vec<TipLeaderboardEntry>, Self::Error> {
            Ok(Vec::new())
        }

        fn get_user_tip_stats(
            &self,
            _discord_id: i64,
            _guild_id: Option<i64>,
        ) -> Result<UserTipStats, Self::Error> {
            Ok(UserTipStats::default())
        }

        fn get_total_tip_volume(&self, _guild_id: Option<i64>) -> Result<TipVolume, Self::Error> {
            Ok(TipVolume::default())
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct AtomicCall {
        from_discord_id: i64,
        to_discord_id: i64,
        guild_id: i64,
        amount: i64,
        fee: i64,
        tithe: i64,
    }

    struct MockPlayerState {
        balances: HashMap<(i64, i64), i64>,
        calls: Vec<AtomicCall>,
    }

    struct MockPlayerRepository {
        state: RefCell<MockPlayerState>,
    }

    impl MockPlayerRepository {
        fn with_balances(guild_id: i64, balances: [(i64, i64); 2]) -> Self {
            Self {
                state: RefCell::new(MockPlayerState {
                    balances: balances
                        .into_iter()
                        .map(|(discord_id, balance)| ((discord_id, guild_id), balance))
                        .collect(),
                    calls: Vec::new(),
                }),
            }
        }

        fn balance(&self, discord_id: i64, guild_id: i64) -> i64 {
            self.state.borrow().balances[&(discord_id, guild_id)]
        }
    }

    impl PlayerTipStore for MockPlayerRepository {
        type Error = String;

        fn tip_atomic(
            &self,
            from_discord_id: i64,
            to_discord_id: i64,
            guild_id: i64,
            amount: i64,
            fee: i64,
            tithe: i64,
        ) -> Result<TipTransferResult, Self::Error> {
            let mut state = self.state.borrow_mut();
            state.calls.push(AtomicCall {
                from_discord_id,
                to_discord_id,
                guild_id,
                amount,
                fee,
                tithe,
            });
            if amount <= 0 {
                return Err("Amount must be positive.".to_owned());
            }
            if fee < 0 {
                return Err("Fee cannot be negative.".to_owned());
            }
            if tithe < 0 {
                return Err("Tithe cannot be negative.".to_owned());
            }
            let total_cost = amount + fee + tithe;
            let sender_balance = state
                .balances
                .get(&(from_discord_id, guild_id))
                .copied()
                .ok_or_else(|| "Sender not found.".to_owned())?;
            let recipient_balance = state
                .balances
                .get(&(to_discord_id, guild_id))
                .copied()
                .ok_or_else(|| "Recipient not found.".to_owned())?;
            if sender_balance < total_cost {
                return Err("Insufficient balance.".to_owned());
            }
            let from_new_balance = sender_balance - total_cost;
            let to_new_balance = recipient_balance + amount;
            state
                .balances
                .insert((from_discord_id, guild_id), from_new_balance);
            state
                .balances
                .insert((to_discord_id, guild_id), to_new_balance);
            Ok(TipTransferResult {
                amount,
                fee,
                tithe,
                nonprofit_credit: fee + tithe,
                from_new_balance,
                to_new_balance,
            })
        }
    }

    #[test]
    fn test_log_tip_delegates_to_repository() {
        let service = TipService::new(MockTipRepository::default());
        let transaction_id = service
            .log_tip(1_001, 1_002, 50, 5, Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(transaction_id, 1);

        let tips = service
            .get_tips_by_sender(1_001, Some(TEST_GUILD_ID), 5)
            .unwrap();
        assert_eq!(tips.len(), 1);
        assert_eq!((tips[0].amount, tips[0].fee), (50, 5));
        assert_eq!(
            service.tip_repo.state.borrow().log_calls,
            [LogCall {
                sender_id: 1_001,
                recipient_id: 1_002,
                amount: 50,
                fee: 5,
                guild_id: Some(TEST_GUILD_ID),
            }]
        );
    }

    #[test]
    fn test_tip_atomic_debits_sender_and_credits_recipient_exactly() {
        let service = PlayerTipService::new(MockPlayerRepository::with_balances(
            TEST_GUILD_ID,
            [(1_001, 500), (1_002, 3)],
        ));
        let sender_before = service.player_repo.balance(1_001, TEST_GUILD_ID);
        let recipient_before = service.player_repo.balance(1_002, TEST_GUILD_ID);

        let result = service
            .tip_atomic(1_001, 1_002, TEST_GUILD_ID, 50, 5)
            .unwrap();

        assert_eq!(
            service.player_repo.balance(1_001, TEST_GUILD_ID),
            sender_before - 55
        );
        assert_eq!(
            service.player_repo.balance(1_002, TEST_GUILD_ID),
            recipient_before + 50
        );
        assert_eq!(result.from_new_balance, sender_before - 55);
        assert_eq!(result.to_new_balance, recipient_before + 50);
        assert_eq!(result.nonprofit_credit, 5);
        assert_eq!(
            service.player_repo.state.borrow().calls,
            [AtomicCall {
                from_discord_id: 1_001,
                to_discord_id: 1_002,
                guild_id: TEST_GUILD_ID,
                amount: 50,
                fee: 5,
                tithe: 0,
            }]
        );
    }

    #[test]
    fn test_get_total_fees_collected_none_guild_same_as_zero() {
        let service = TipService::new(MockTipRepository::default());
        service.log_tip(1, 2, 10, 2, None).unwrap();

        assert_eq!(service.get_total_fees_collected(None).unwrap(), 2);
        assert_eq!(service.get_total_fees_collected(Some(0)).unwrap(), 2);
        assert_eq!(
            service.tip_repo.state.borrow().fee_query_calls,
            [None, Some(0)]
        );
        assert_eq!(service.tip_repo.state.borrow().log_calls[0].guild_id, None);
    }

    #[test]
    fn tithe_is_forwarded_without_service_layer_recalculation() {
        let service = PlayerTipService::new(MockPlayerRepository::with_balances(
            TEST_GUILD_ID,
            [(1_001, 500), (1_002, 3)],
        ));
        let result = service
            .tip_atomic_with_tithe(1_001, 1_002, TEST_GUILD_ID, 50, 5, 3)
            .unwrap();
        assert_eq!(result.from_new_balance, 442);
        assert_eq!(result.to_new_balance, 53);
        assert_eq!(result.nonprofit_credit, 8);
        assert_eq!(service.player_repo.state.borrow().calls[0].tithe, 3);
    }

    #[test]
    fn repository_errors_pass_through_unchanged() {
        let service = PlayerTipService::new(MockPlayerRepository::with_balances(
            TEST_GUILD_ID,
            [(1_001, 5), (1_002, 3)],
        ));
        assert_eq!(
            service
                .tip_atomic(1_001, 1_002, TEST_GUILD_ID, 50, 5)
                .unwrap_err(),
            "Insufficient balance."
        );
    }
}
