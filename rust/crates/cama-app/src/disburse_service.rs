//! Typed, adapter-neutral Jopacoin Reserve governance service.
//!
//! Python remains the production implementation and the only live SQLite and
//! Discord writer during the side-by-side migration.  This module ports the
//! service/repository contract onto a transactional in-memory adapter so the
//! proposal state machine, eligibility rules, reserve conservation, ballot
//! archive, voting restrictions, and concurrent execution boundaries can be
//! exercised without opening `cama_shuffle.db`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

pub type GuildId = i64;
pub type DiscordId = i64;

pub const LOTTERY_ACTIVITY_DAYS: i64 = 14;
pub const MONETARY_RECOVERY_CODE: &str = "monetary_recovery";
pub const ECONOMY_EDICT_CODE: &str = "economy_edict";
pub const MONETARY_RECOVERY_REASON: &str = "Jopacoin Reserve voting is temporarily disabled while the economy is in monetary recovery mode.";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DistributionMethod {
    Even,
    Proportional,
    Neediest,
    Stimulus,
    Lottery,
    SocialSecurity,
    Richest,
    Burn,
    NextMatchPot,
    Cancel,
}

impl DistributionMethod {
    pub const ALL: [Self; 10] = [
        Self::Even,
        Self::Proportional,
        Self::Neediest,
        Self::Stimulus,
        Self::Lottery,
        Self::SocialSecurity,
        Self::Richest,
        Self::Burn,
        Self::NextMatchPot,
        Self::Cancel,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Even => "even",
            Self::Proportional => "proportional",
            Self::Neediest => "neediest",
            Self::Stimulus => "stimulus",
            Self::Lottery => "lottery",
            Self::SocialSecurity => "social_security",
            Self::Richest => "richest",
            Self::Burn => "burn",
            Self::NextMatchPot => "next_match_pot",
            Self::Cancel => "cancel",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Even => "Even Split",
            Self::Proportional => "Proportional",
            Self::Neediest => "Neediest First",
            Self::Stimulus => "Stimulus",
            Self::Lottery => "Lottery",
            Self::SocialSecurity => "Social Security",
            Self::Richest => "Richest",
            Self::Burn => "Burn",
            Self::NextMatchPot => "Next Match Pot",
            Self::Cancel => "Cancel",
        }
    }
}

impl fmt::Display for DistributionMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProposalStatus {
    Active,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub guild_id: GuildId,
    pub proposal_id: i64,
    pub message_id: Option<u64>,
    pub channel_id: Option<u64>,
    pub fund_amount: i64,
    pub quorum_required: u32,
    pub status: ProposalStatus,
    pub votes: BTreeMap<DistributionMethod, u32>,
}

impl Proposal {
    #[must_use]
    pub fn total_votes(&self) -> u32 {
        self.votes.values().sum()
    }

    #[must_use]
    pub fn vote_count(&self, method: DistributionMethod) -> u32 {
        self.votes.get(&method).copied().unwrap_or(0)
    }

    #[must_use]
    pub fn quorum_reached(&self) -> bool {
        self.total_votes() >= self.quorum_required
    }

    #[must_use]
    pub fn quorum_progress(&self) -> f64 {
        if self.quorum_required == 0 {
            1.0
        } else {
            f64::from(self.total_votes()) / f64::from(self.quorum_required)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Distribution {
    pub discord_id: DiscordId,
    pub amount: i64,
}

impl Distribution {
    #[must_use]
    pub const fn new(discord_id: DiscordId, amount: i64) -> Self {
        Self { discord_id, amount }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoteRecord {
    pub discord_id: DiscordId,
    pub method: DistributionMethod,
    pub voted_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchivedBallot {
    pub guild_id: GuildId,
    pub proposal_id: i64,
    pub discord_id: DiscordId,
    pub method: DistributionMethod,
    pub voted_at: i64,
    pub proposal_outcome: String,
    pub finalized_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisbursementHistory {
    pub guild_id: GuildId,
    pub disbursed_at: i64,
    pub total_amount: i64,
    pub method: DistributionMethod,
    pub recipients: Vec<Distribution>,
}

impl DisbursementHistory {
    #[must_use]
    pub fn recipient_count(&self) -> usize {
        self.recipients.len()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerEntry {
    pub guild_id: GuildId,
    pub delta: i64,
    pub source: &'static str,
    pub related_id: i64,
    pub reason: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerSnapshot {
    pub discord_id: DiscordId,
    pub username: String,
    pub balance: i64,
    pub wins: u32,
    pub losses: u32,
    pub last_match_at: Option<i64>,
}

impl PlayerSnapshot {
    #[must_use]
    pub const fn games_played(&self) -> u32 {
        self.wins.saturating_add(self.losses)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Debtor {
    pub discord_id: DiscordId,
    pub balance: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StimulusCandidate {
    pub discord_id: DiscordId,
    pub balance: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GamesCandidate {
    pub discord_id: DiscordId,
    pub games_played: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RichestCandidate {
    pub discord_id: DiscordId,
    pub balance: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EconomyEdict {
    pub event_id: i64,
    pub name: String,
    pub severity: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VotingRestriction {
    pub code: &'static str,
    pub reason: String,
    pub edict: Option<EconomyEdict>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProposalBlock {
    MonetaryRecovery,
    EconomyEdict,
    ActiveProposalExists,
    InsufficientFund { available: i64, minimum: i64 },
}

impl ProposalBlock {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MonetaryRecovery => MONETARY_RECOVERY_CODE,
            Self::EconomyEdict => ECONOMY_EDICT_CODE,
            Self::ActiveProposalExists => "active_proposal_exists",
            Self::InsufficientFund { .. } => "insufficient_fund",
        }
    }

    #[must_use]
    pub fn python_reason(&self) -> String {
        match self {
            Self::InsufficientFund { available, minimum } => {
                format!("insufficient_fund:{available}:{minimum}")
            }
            other => other.code().to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProposalEligibility {
    Allowed,
    Blocked(ProposalBlock),
}

impl ProposalEligibility {
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }

    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Allowed => String::new(),
            Self::Blocked(reason) => reason.python_reason(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisburseError {
    VotingRestricted(VotingRestriction),
    CannotCreate(ProposalBlock),
    NoActiveProposal,
    QuorumNotReached,
    NoVotes,
    InsufficientFund { available: i64, needed: i64 },
    MissingRecipient { discord_id: DiscordId },
    InvalidAmount(i64),
}

impl fmt::Display for DisburseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VotingRestricted(restriction) => formatter.write_str(&restriction.reason),
            Self::CannotCreate(reason) => {
                write!(
                    formatter,
                    "Cannot create proposal: {}",
                    reason.python_reason()
                )
            }
            Self::NoActiveProposal => formatter.write_str("No active proposal"),
            Self::QuorumNotReached => formatter.write_str("Quorum not reached"),
            Self::NoVotes => formatter.write_str("No votes have been cast yet"),
            Self::InsufficientFund { available, needed } => write!(
                formatter,
                "Insufficient funds in nonprofit. Available: {available}, needed: {needed}"
            ),
            Self::MissingRecipient { discord_id } => write!(
                formatter,
                "Disbursement aborted: recipient row is missing for {discord_id}"
            ),
            Self::InvalidAmount(amount) => write!(formatter, "Invalid monetary amount: {amount}"),
        }
    }
}

impl std::error::Error for DisburseError {}

#[derive(Clone, Debug, PartialEq)]
pub struct VoteState {
    pub votes: BTreeMap<DistributionMethod, u32>,
    pub total_votes: u32,
    pub quorum_required: u32,
    pub quorum_reached: bool,
    pub quorum_progress: f64,
}

impl VoteState {
    #[must_use]
    pub fn vote_count(&self, method: DistributionMethod) -> u32 {
        self.votes.get(&method).copied().unwrap_or(0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionResult {
    pub success: bool,
    pub method: DistributionMethod,
    pub method_label: &'static str,
    pub total_disbursed: i64,
    pub distributions: Vec<Distribution>,
    pub recipient_count: usize,
    pub cancelled: bool,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MoratoriumResult {
    pub cancelled: bool,
    pub proposal_id: Option<i64>,
    pub fund_amount_returned: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct FundAccount {
    available: i64,
    next_match_pot: i64,
}

#[derive(Clone, Debug)]
struct RepositoryState {
    players: BTreeMap<(GuildId, DiscordId), PlayerSnapshot>,
    funds: BTreeMap<GuildId, FundAccount>,
    active_proposals: BTreeMap<GuildId, Proposal>,
    votes: BTreeMap<(GuildId, i64, DiscordId), VoteRecord>,
    archived_ballots: Vec<ArchivedBallot>,
    history: Vec<DisbursementHistory>,
    ledger: Vec<LedgerEntry>,
    clock: i64,
    activity_now: i64,
    random_state: u64,
}

impl Default for RepositoryState {
    fn default() -> Self {
        Self {
            players: BTreeMap::new(),
            funds: BTreeMap::new(),
            active_proposals: BTreeMap::new(),
            votes: BTreeMap::new(),
            archived_ballots: Vec::new(),
            history: Vec::new(),
            ledger: Vec::new(),
            clock: 1_700_000_000,
            activity_now: 2_000_000_000,
            random_state: 0x4d59_5df4_d0f3_3173,
        }
    }
}

impl RepositoryState {
    fn timestamp(&mut self) -> i64 {
        let value = self.clock;
        self.clock = self.clock.saturating_add(1);
        value
    }

    fn random_index(&mut self, upper_bound: usize) -> usize {
        debug_assert!(upper_bound > 0);
        self.random_state = self
            .random_state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let upper = u64::try_from(upper_bound).unwrap_or(u64::MAX);
        usize::try_from(self.random_state % upper).unwrap_or(0)
    }
}

/// Shared transactional adapter used by the Rust parity tranche.
///
/// Every write closure snapshots state and restores it on error.  The mutex is
/// held for the complete closure, matching the service's per-guild critical
/// section plus the repository's `BEGIN IMMEDIATE` all-or-nothing boundary.
#[derive(Clone, Debug, Default)]
pub struct InMemoryDisburseRepository {
    inner: Arc<Mutex<RepositoryState>>,
}

impl InMemoryDisburseRepository {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, RepositoryState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read<T>(&self, read: impl FnOnce(&RepositoryState) -> T) -> T {
        read(&self.lock())
    }

    fn transaction<T>(
        &self,
        write: impl FnOnce(&mut RepositoryState) -> Result<T, DisburseError>,
    ) -> Result<T, DisburseError> {
        let mut state = self.lock();
        let before = state.clone();
        match write(&mut state) {
            Ok(value) => Ok(value),
            Err(error) => {
                *state = before;
                Err(error)
            }
        }
    }

    pub fn set_clock(&self, timestamp: i64) {
        self.lock().clock = timestamp;
    }

    pub fn set_activity_now(&self, timestamp: i64) {
        self.lock().activity_now = timestamp;
    }

    pub fn add_player(
        &self,
        guild_id: GuildId,
        discord_id: DiscordId,
        username: impl Into<String>,
    ) {
        self.lock().players.insert(
            (guild_id, discord_id),
            PlayerSnapshot {
                discord_id,
                username: username.into(),
                balance: 0,
                wins: 0,
                losses: 0,
                last_match_at: None,
            },
        );
    }

    pub fn remove_player(&self, guild_id: GuildId, discord_id: DiscordId) {
        self.lock().players.remove(&(guild_id, discord_id));
    }

    pub fn set_balance(
        &self,
        guild_id: GuildId,
        discord_id: DiscordId,
        balance: i64,
    ) -> Result<(), DisburseError> {
        self.transaction(|state| {
            let player = state
                .players
                .get_mut(&(guild_id, discord_id))
                .ok_or(DisburseError::MissingRecipient { discord_id })?;
            player.balance = balance;
            Ok(())
        })
    }

    #[must_use]
    pub fn balance(&self, guild_id: GuildId, discord_id: DiscordId) -> Option<i64> {
        self.read(|state| {
            state
                .players
                .get(&(guild_id, discord_id))
                .map(|player| player.balance)
        })
    }

    pub fn increment_wins(
        &self,
        guild_id: GuildId,
        discord_id: DiscordId,
    ) -> Result<(), DisburseError> {
        self.transaction(|state| {
            let player = state
                .players
                .get_mut(&(guild_id, discord_id))
                .ok_or(DisburseError::MissingRecipient { discord_id })?;
            player.wins = player.wins.saturating_add(1);
            Ok(())
        })
    }

    pub fn set_games(
        &self,
        guild_id: GuildId,
        discord_id: DiscordId,
        wins: u32,
        losses: u32,
    ) -> Result<(), DisburseError> {
        self.transaction(|state| {
            let player = state
                .players
                .get_mut(&(guild_id, discord_id))
                .ok_or(DisburseError::MissingRecipient { discord_id })?;
            player.wins = wins;
            player.losses = losses;
            Ok(())
        })
    }

    pub fn set_last_match_at(
        &self,
        guild_id: GuildId,
        discord_id: DiscordId,
        timestamp: i64,
    ) -> Result<(), DisburseError> {
        self.transaction(|state| {
            let player = state
                .players
                .get_mut(&(guild_id, discord_id))
                .ok_or(DisburseError::MissingRecipient { discord_id })?;
            player.last_match_at = Some(timestamp);
            Ok(())
        })
    }

    pub fn add_to_fund(&self, guild_id: GuildId, amount: i64) -> Result<i64, DisburseError> {
        if amount < 0 {
            return Err(DisburseError::InvalidAmount(amount));
        }
        self.transaction(|state| {
            let account = state.funds.entry(guild_id).or_default();
            account.available = account.available.saturating_add(amount);
            Ok(account.available)
        })
    }

    #[must_use]
    pub fn fund(&self, guild_id: GuildId) -> i64 {
        self.read(|state| {
            state
                .funds
                .get(&guild_id)
                .map_or(0, |account| account.available)
        })
    }

    #[must_use]
    pub fn next_match_pot(&self, guild_id: GuildId) -> i64 {
        self.read(|state| {
            state
                .funds
                .get(&guild_id)
                .map_or(0, |account| account.next_match_pot)
        })
    }

    #[must_use]
    pub fn registered_player_count(&self, guild_id: GuildId) -> usize {
        self.read(|state| registered_player_count(state, guild_id))
    }

    #[must_use]
    pub fn players_with_negative_balance(&self, guild_id: GuildId) -> Vec<Debtor> {
        self.read(|state| players_with_negative_balance(state, guild_id))
    }

    #[must_use]
    pub fn stimulus_eligible_players(&self, guild_id: GuildId) -> Vec<StimulusCandidate> {
        self.read(|state| stimulus_eligible_players(state, guild_id))
    }

    #[must_use]
    pub fn lottery_eligible_players(
        &self,
        guild_id: GuildId,
        activity_days: i64,
    ) -> Vec<DiscordId> {
        self.read(|state| lottery_eligible_players(state, guild_id, activity_days))
    }

    #[must_use]
    pub fn players_by_games_played(&self, guild_id: GuildId) -> Vec<GamesCandidate> {
        self.read(|state| players_by_games_played(state, guild_id))
    }

    #[must_use]
    pub fn richest_player(&self, guild_id: GuildId) -> Option<RichestCandidate> {
        self.read(|state| richest_player(state, guild_id))
    }

    #[must_use]
    pub fn active_proposal(&self, guild_id: GuildId) -> Option<Proposal> {
        self.read(|state| proposal_with_votes(state, guild_id))
    }

    #[must_use]
    pub fn individual_votes(&self, guild_id: GuildId) -> Vec<VoteRecord> {
        self.read(|state| individual_votes(state, guild_id))
    }

    #[must_use]
    pub fn archived_ballots(&self, guild_id: GuildId, proposal_id: i64) -> Vec<ArchivedBallot> {
        self.read(|state| {
            let mut ballots: Vec<_> = state
                .archived_ballots
                .iter()
                .filter(|ballot| ballot.guild_id == guild_id && ballot.proposal_id == proposal_id)
                .cloned()
                .collect();
            ballots.sort_by_key(|ballot| ballot.discord_id);
            ballots
        })
    }

    #[must_use]
    pub fn active_vote_count(&self, guild_id: GuildId, proposal_id: i64) -> usize {
        self.read(|state| {
            state
                .votes
                .keys()
                .filter(|(vote_guild, vote_proposal, _)| {
                    *vote_guild == guild_id && *vote_proposal == proposal_id
                })
                .count()
        })
    }

    #[must_use]
    pub fn last_disbursement(&self, guild_id: GuildId) -> Option<DisbursementHistory> {
        self.read(|state| {
            state
                .history
                .iter()
                .rev()
                .find(|entry| entry.guild_id == guild_id)
                .cloned()
        })
    }

    #[must_use]
    pub fn ledger_entries(&self, guild_id: GuildId, related_id: i64) -> Vec<LedgerEntry> {
        self.read(|state| {
            state
                .ledger
                .iter()
                .filter(|entry| entry.guild_id == guild_id && entry.related_id == related_id)
                .cloned()
                .collect()
        })
    }

    /// Repository-level atomic completion hook used to exercise rollback when
    /// a recipient disappears after proposal creation.
    pub fn complete_and_disburse_atomic(
        &self,
        guild_id: GuildId,
        fund_amount_to_return: i64,
        distributions: &[Distribution],
        method: DistributionMethod,
    ) -> Result<i64, DisburseError> {
        self.transaction(|state| {
            complete_and_disburse(
                state,
                guild_id,
                fund_amount_to_return,
                distributions,
                method,
            )
        })
    }
}

type EdictProvider = Arc<dyn Fn(GuildId) -> Option<EconomyEdict> + Send + Sync>;

#[derive(Clone)]
pub struct DisburseService {
    repository: InMemoryDisburseRepository,
    min_fund: i64,
    quorum_percentage: f64,
    voting_enabled: bool,
    edict_provider: Option<EdictProvider>,
}

impl fmt::Debug for DisburseService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisburseService")
            .field("repository", &self.repository)
            .field("min_fund", &self.min_fund)
            .field("quorum_percentage", &self.quorum_percentage)
            .field("voting_enabled", &self.voting_enabled)
            .field("has_edict_provider", &self.edict_provider.is_some())
            .finish()
    }
}

impl DisburseService {
    #[must_use]
    pub fn new(
        repository: InMemoryDisburseRepository,
        min_fund: i64,
        quorum_percentage: f64,
    ) -> Self {
        Self {
            repository,
            min_fund,
            quorum_percentage,
            voting_enabled: true,
            edict_provider: None,
        }
    }

    #[must_use]
    pub fn with_voting_enabled(mut self, enabled: bool) -> Self {
        self.voting_enabled = enabled;
        self
    }

    #[must_use]
    pub fn with_edict_provider(
        mut self,
        provider: impl Fn(GuildId) -> Option<EconomyEdict> + Send + Sync + 'static,
    ) -> Self {
        self.edict_provider = Some(Arc::new(provider));
        self
    }

    #[must_use]
    pub fn repository(&self) -> &InMemoryDisburseRepository {
        &self.repository
    }

    #[must_use]
    pub fn get_voting_restriction(&self, guild_id: GuildId) -> Option<VotingRestriction> {
        if !self.voting_enabled {
            return Some(VotingRestriction {
                code: MONETARY_RECOVERY_CODE,
                reason: MONETARY_RECOVERY_REASON.to_owned(),
                edict: None,
            });
        }
        let edict = self.edict_provider.as_ref()?.as_ref()(guild_id)?;
        let severity = edict.severity.clamp(1, 5);
        let roman = ["I", "II", "III", "IV", "V"][usize::from(severity - 1)];
        Some(VotingRestriction {
            code: ECONOMY_EDICT_CODE,
            reason: format!(
                "Jopacoin Reserve allocation voting is suspended by {} — Level {roman}.",
                edict.name
            ),
            edict: Some(EconomyEdict { severity, ..edict }),
        })
    }

    fn require_voting_enabled(&self, guild_id: GuildId) -> Result<(), DisburseError> {
        self.get_voting_restriction(guild_id)
            .map_or(Ok(()), |restriction| {
                Err(DisburseError::VotingRestricted(restriction))
            })
    }

    #[must_use]
    pub fn can_propose(&self, guild_id: GuildId) -> ProposalEligibility {
        if let Some(restriction) = self.get_voting_restriction(guild_id) {
            let block = match restriction.code {
                MONETARY_RECOVERY_CODE => ProposalBlock::MonetaryRecovery,
                _ => ProposalBlock::EconomyEdict,
            };
            return ProposalEligibility::Blocked(block);
        }
        self.repository.read(|state| {
            if state.active_proposals.contains_key(&guild_id) {
                return ProposalEligibility::Blocked(ProposalBlock::ActiveProposalExists);
            }
            let available = state
                .funds
                .get(&guild_id)
                .map_or(0, |account| account.available);
            if available < self.min_fund {
                return ProposalEligibility::Blocked(ProposalBlock::InsufficientFund {
                    available,
                    minimum: self.min_fund,
                });
            }
            ProposalEligibility::Allowed
        })
    }

    pub fn create_proposal(&self, guild_id: GuildId) -> Result<Proposal, DisburseError> {
        self.require_voting_enabled(guild_id)?;
        self.repository.transaction(|state| {
            if state.active_proposals.contains_key(&guild_id) {
                return Err(DisburseError::CannotCreate(
                    ProposalBlock::ActiveProposalExists,
                ));
            }
            let available = state
                .funds
                .get(&guild_id)
                .map_or(0, |account| account.available);
            if available < self.min_fund {
                return Err(DisburseError::CannotCreate(
                    ProposalBlock::InsufficientFund {
                        available,
                        minimum: self.min_fund,
                    },
                ));
            }

            let player_count = registered_player_count(state, guild_id);
            let quorum = ((player_count as f64 * self.quorum_percentage).ceil() as u32).max(1);
            let proposal_id = state.timestamp();
            state.funds.entry(guild_id).or_default().available = 0;
            let proposal = Proposal {
                guild_id,
                proposal_id,
                message_id: None,
                channel_id: None,
                fund_amount: available,
                quorum_required: quorum,
                status: ProposalStatus::Active,
                votes: empty_vote_counts(),
            };
            state.active_proposals.insert(guild_id, proposal.clone());
            Ok(proposal)
        })
    }

    pub fn set_proposal_message(
        &self,
        guild_id: GuildId,
        message_id: u64,
        channel_id: u64,
    ) -> Result<(), DisburseError> {
        self.repository.transaction(|state| {
            let proposal = state
                .active_proposals
                .get_mut(&guild_id)
                .ok_or(DisburseError::NoActiveProposal)?;
            proposal.message_id = Some(message_id);
            proposal.channel_id = Some(channel_id);
            Ok(())
        })
    }

    #[must_use]
    pub fn get_proposal(&self, guild_id: GuildId) -> Option<Proposal> {
        self.repository.active_proposal(guild_id)
    }

    pub fn add_vote(
        &self,
        guild_id: GuildId,
        discord_id: DiscordId,
        method: DistributionMethod,
    ) -> Result<VoteState, DisburseError> {
        self.require_voting_enabled(guild_id)?;
        self.repository.transaction(|state| {
            let proposal = state
                .active_proposals
                .get(&guild_id)
                .cloned()
                .ok_or(DisburseError::NoActiveProposal)?;
            let voted_at = state.timestamp();
            state.votes.insert(
                (guild_id, proposal.proposal_id, discord_id),
                VoteRecord {
                    discord_id,
                    method,
                    voted_at,
                },
            );
            let votes = vote_counts(state, guild_id, proposal.proposal_id);
            let total_votes = votes.values().sum();
            Ok(VoteState {
                votes,
                total_votes,
                quorum_required: proposal.quorum_required,
                quorum_reached: total_votes >= proposal.quorum_required,
                quorum_progress: if proposal.quorum_required == 0 {
                    1.0
                } else {
                    f64::from(total_votes) / f64::from(proposal.quorum_required)
                },
            })
        })
    }

    #[must_use]
    pub fn check_quorum(&self, guild_id: GuildId) -> (bool, Option<DistributionMethod>) {
        let Some(proposal) = self.get_proposal(guild_id) else {
            return (false, None);
        };
        if !proposal.quorum_reached() {
            return (false, None);
        }
        (true, determine_winner(&proposal.votes))
    }

    pub fn execute_disbursement(
        &self,
        guild_id: GuildId,
    ) -> Result<ExecutionResult, DisburseError> {
        self.require_voting_enabled(guild_id)?;
        self.repository.transaction(|state| {
            let proposal =
                proposal_with_votes(state, guild_id).ok_or(DisburseError::NoActiveProposal)?;
            if !proposal.quorum_reached() {
                return Err(DisburseError::QuorumNotReached);
            }
            let method = determine_winner(&proposal.votes).ok_or(DisburseError::NoVotes)?;
            execute_locked(state, guild_id, &proposal, method, false)
        })
    }

    pub fn force_execute(&self, guild_id: GuildId) -> Result<ExecutionResult, DisburseError> {
        self.require_voting_enabled(guild_id)?;
        self.repository.transaction(|state| {
            let proposal =
                proposal_with_votes(state, guild_id).ok_or(DisburseError::NoActiveProposal)?;
            let method = determine_winner(&proposal.votes).ok_or(DisburseError::NoVotes)?;
            execute_locked(state, guild_id, &proposal, method, true)
        })
    }

    pub fn reset_proposal(&self, guild_id: GuildId) -> Result<bool, DisburseError> {
        self.repository.transaction(|state| {
            let Some(proposal) = state.active_proposals.get(&guild_id).cloned() else {
                return Ok(false);
            };
            archive_final_votes(state, guild_id, proposal.proposal_id, "reset");
            state.active_proposals.remove(&guild_id);
            remove_active_votes(state, guild_id, proposal.proposal_id);
            state.funds.entry(guild_id).or_default().available = state
                .funds
                .get(&guild_id)
                .map_or(proposal.fund_amount, |account| {
                    account.available.saturating_add(proposal.fund_amount)
                });
            Ok(true)
        })
    }

    pub fn enforce_voting_moratorium(
        &self,
        guild_id: GuildId,
    ) -> Result<MoratoriumResult, DisburseError> {
        self.repository.transaction(|state| {
            let Some(proposal) = state.active_proposals.get(&guild_id).cloned() else {
                return Ok(MoratoriumResult {
                    cancelled: false,
                    proposal_id: None,
                    fund_amount_returned: 0,
                });
            };
            archive_final_votes(
                state,
                guild_id,
                proposal.proposal_id,
                MONETARY_RECOVERY_CODE,
            );
            state.active_proposals.remove(&guild_id);
            remove_active_votes(state, guild_id, proposal.proposal_id);
            let account = state.funds.entry(guild_id).or_default();
            account.available = account.available.saturating_add(proposal.fund_amount);
            let mut metadata = BTreeMap::new();
            metadata.insert(
                "proposal_outcome".to_owned(),
                MONETARY_RECOVERY_CODE.to_owned(),
            );
            metadata.insert("fund_amount".to_owned(), proposal.fund_amount.to_string());
            state.ledger.push(LedgerEntry {
                guild_id,
                delta: proposal.fund_amount,
                source: "disburse",
                related_id: proposal.proposal_id,
                reason: "Jopacoin Reserve proposal cancelled for monetary recovery; locked funds returned".to_owned(),
                metadata,
            });
            Ok(MoratoriumResult {
                cancelled: true,
                proposal_id: Some(proposal.proposal_id),
                fund_amount_returned: proposal.fund_amount,
            })
        })
    }

    #[must_use]
    pub fn get_last_disbursement(&self, guild_id: GuildId) -> Option<DisbursementHistory> {
        self.repository.last_disbursement(guild_id)
    }

    #[must_use]
    pub fn get_individual_votes(&self, guild_id: GuildId) -> Vec<VoteRecord> {
        self.repository.individual_votes(guild_id)
    }
}

fn empty_vote_counts() -> BTreeMap<DistributionMethod, u32> {
    DistributionMethod::ALL
        .into_iter()
        .map(|method| (method, 0))
        .collect()
}

fn registered_player_count(state: &RepositoryState, guild_id: GuildId) -> usize {
    state
        .players
        .keys()
        .filter(|(player_guild, _)| *player_guild == guild_id)
        .count()
}

fn vote_counts(
    state: &RepositoryState,
    guild_id: GuildId,
    proposal_id: i64,
) -> BTreeMap<DistributionMethod, u32> {
    let mut counts = empty_vote_counts();
    for ((vote_guild, vote_proposal, _), vote) in &state.votes {
        if *vote_guild == guild_id && *vote_proposal == proposal_id {
            counts
                .entry(vote.method)
                .and_modify(|count| *count = count.saturating_add(1));
        }
    }
    counts
}

fn proposal_with_votes(state: &RepositoryState, guild_id: GuildId) -> Option<Proposal> {
    let mut proposal = state.active_proposals.get(&guild_id)?.clone();
    proposal.votes = vote_counts(state, guild_id, proposal.proposal_id);
    Some(proposal)
}

fn determine_winner(votes: &BTreeMap<DistributionMethod, u32>) -> Option<DistributionMethod> {
    let maximum = votes.values().copied().max().unwrap_or(0);
    if maximum == 0 {
        return None;
    }
    DistributionMethod::ALL
        .into_iter()
        .find(|method| votes.get(method).copied().unwrap_or(0) == maximum)
}

fn individual_votes(state: &RepositoryState, guild_id: GuildId) -> Vec<VoteRecord> {
    let Some(proposal) = state.active_proposals.get(&guild_id) else {
        return Vec::new();
    };
    let mut votes: Vec<_> = state
        .votes
        .iter()
        .filter(|((vote_guild, vote_proposal, _), _)| {
            *vote_guild == guild_id && *vote_proposal == proposal.proposal_id
        })
        .map(|(_, vote)| vote.clone())
        .collect();
    votes.sort_by_key(|vote| (vote.voted_at, vote.discord_id));
    votes
}

fn archive_final_votes(
    state: &mut RepositoryState,
    guild_id: GuildId,
    proposal_id: i64,
    proposal_outcome: &str,
) {
    let finalized_at = state.timestamp();
    let records: Vec<_> = state
        .votes
        .iter()
        .filter(|((vote_guild, vote_proposal, _), _)| {
            *vote_guild == guild_id && *vote_proposal == proposal_id
        })
        .map(|(_, vote)| vote.clone())
        .collect();
    for vote in records {
        state.archived_ballots.push(ArchivedBallot {
            guild_id,
            proposal_id,
            discord_id: vote.discord_id,
            method: vote.method,
            voted_at: vote.voted_at,
            proposal_outcome: proposal_outcome.to_owned(),
            finalized_at,
        });
    }
}

fn remove_active_votes(state: &mut RepositoryState, guild_id: GuildId, proposal_id: i64) {
    state.votes.retain(|(vote_guild, vote_proposal, _), _| {
        *vote_guild != guild_id || *vote_proposal != proposal_id
    });
}

fn complete_and_disburse(
    state: &mut RepositoryState,
    guild_id: GuildId,
    fund_amount_to_return: i64,
    distributions: &[Distribution],
    method: DistributionMethod,
) -> Result<i64, DisburseError> {
    if fund_amount_to_return < 0 || distributions.iter().any(|entry| entry.amount < 0) {
        return Err(DisburseError::InvalidAmount(fund_amount_to_return));
    }
    let proposal = state
        .active_proposals
        .get(&guild_id)
        .cloned()
        .ok_or(DisburseError::NoActiveProposal)?;
    let total = distributions
        .iter()
        .try_fold(0_i64, |sum, entry| sum.checked_add(entry.amount))
        .ok_or(DisburseError::InvalidAmount(i64::MAX))?;

    archive_final_votes(state, guild_id, proposal.proposal_id, method.as_str());
    state.active_proposals.remove(&guild_id);
    remove_active_votes(state, guild_id, proposal.proposal_id);
    let account = state.funds.entry(guild_id).or_default();
    account.available = account.available.saturating_add(fund_amount_to_return);
    if account.available < total {
        return Err(DisburseError::InsufficientFund {
            available: account.available,
            needed: total,
        });
    }
    account.available -= total;

    for distribution in distributions {
        let player = state
            .players
            .get_mut(&(guild_id, distribution.discord_id))
            .ok_or(DisburseError::MissingRecipient {
                discord_id: distribution.discord_id,
            })?;
        player.balance = player.balance.saturating_add(distribution.amount);
    }

    if !distributions.is_empty() {
        let disbursed_at = state.timestamp();
        state.history.push(DisbursementHistory {
            guild_id,
            disbursed_at,
            total_amount: total,
            method,
            recipients: distributions.to_vec(),
        });
    }
    Ok(total)
}

fn execute_locked(
    state: &mut RepositoryState,
    guild_id: GuildId,
    proposal: &Proposal,
    method: DistributionMethod,
    forced: bool,
) -> Result<ExecutionResult, DisburseError> {
    match method {
        DistributionMethod::Burn | DistributionMethod::NextMatchPot => {
            let active = state
                .active_proposals
                .get(&guild_id)
                .filter(|active| active.proposal_id == proposal.proposal_id)
                .ok_or(DisburseError::NoActiveProposal)?;
            let proposal_id = active.proposal_id;
            archive_final_votes(state, guild_id, proposal_id, method.as_str());
            state.active_proposals.remove(&guild_id);
            remove_active_votes(state, guild_id, proposal_id);
            if method == DistributionMethod::NextMatchPot {
                let account = state.funds.entry(guild_id).or_default();
                account.next_match_pot =
                    account.next_match_pot.saturating_add(proposal.fund_amount);
            }
            let disbursed_at = state.timestamp();
            state.history.push(DisbursementHistory {
                guild_id,
                disbursed_at,
                total_amount: proposal.fund_amount,
                method,
                recipients: Vec::new(),
            });
            let destination = if method == DistributionMethod::Burn {
                "removed from circulation"
            } else {
                "queued for an even split into the next match's betting pot"
            };
            return Ok(ExecutionResult {
                success: true,
                method,
                method_label: method.label(),
                total_disbursed: proposal.fund_amount,
                distributions: Vec::new(),
                recipient_count: 0,
                cancelled: false,
                message: Some(format!("{} Jopacoin {destination}.", proposal.fund_amount)),
            });
        }
        DistributionMethod::Cancel => {
            archive_final_votes(state, guild_id, proposal.proposal_id, "cancelled");
            state.active_proposals.remove(&guild_id);
            remove_active_votes(state, guild_id, proposal.proposal_id);
            let account = state.funds.entry(guild_id).or_default();
            account.available = account.available.saturating_add(proposal.fund_amount);
            return Ok(ExecutionResult {
                success: true,
                method,
                method_label: method.label(),
                total_disbursed: 0,
                distributions: Vec::new(),
                recipient_count: 0,
                cancelled: true,
                message: Some(if forced {
                    "Proposal cancelled by override. Funds returned to nonprofit.".to_owned()
                } else {
                    "Proposal cancelled by vote. Funds returned to nonprofit.".to_owned()
                }),
            });
        }
        _ => {}
    }

    let (distributions, empty_message) = match method {
        DistributionMethod::Stimulus => {
            let eligible = stimulus_eligible_players(state, guild_id);
            let distributions = calculate_stimulus_distribution(proposal.fund_amount, &eligible);
            let message = distributions.is_empty().then(|| {
                if forced {
                    "No eligible players for stimulus.".to_owned()
                } else {
                    "No eligible players for stimulus (need 4+ non-debtor players).".to_owned()
                }
            });
            (distributions, message)
        }
        DistributionMethod::Lottery => {
            let eligible = lottery_eligible_players(state, guild_id, LOTTERY_ACTIVITY_DAYS);
            let distributions = if eligible.is_empty() {
                Vec::new()
            } else {
                let draw = u64::try_from(state.random_index(eligible.len())).unwrap_or(0);
                calculate_lottery_distribution(proposal.fund_amount, &eligible, draw)
            };
            let message = distributions.is_empty().then(|| {
                if forced {
                    format!(
                        "No active players for lottery (last {LOTTERY_ACTIVITY_DAYS} days)."
                    )
                } else {
                    format!(
                        "No active players for lottery (must have played in the last {LOTTERY_ACTIVITY_DAYS} days)."
                    )
                }
            });
            (distributions, message)
        }
        DistributionMethod::SocialSecurity => {
            let eligible = players_by_games_played(state, guild_id);
            let distributions =
                calculate_social_security_distribution(proposal.fund_amount, &eligible);
            let message = distributions
                .is_empty()
                .then(|| "No players with games played for social security.".to_owned());
            (distributions, message)
        }
        DistributionMethod::Richest => {
            let richest = richest_player(state, guild_id);
            let distributions = calculate_richest_distribution(proposal.fund_amount, richest);
            let message = distributions
                .is_empty()
                .then(|| "No players found for richest distribution.".to_owned());
            (distributions, message)
        }
        DistributionMethod::Even
        | DistributionMethod::Proportional
        | DistributionMethod::Neediest => {
            let debtors = players_with_negative_balance(state, guild_id);
            let distributions = match method {
                DistributionMethod::Even => {
                    calculate_even_distribution(proposal.fund_amount, &debtors)
                }
                DistributionMethod::Proportional => {
                    calculate_proportional_distribution(proposal.fund_amount, &debtors)
                }
                DistributionMethod::Neediest => {
                    calculate_neediest_distribution(proposal.fund_amount, &debtors)
                }
                _ => unreachable!(),
            };
            let message = distributions
                .is_empty()
                .then(|| "No players with negative balance to receive funds.".to_owned());
            (distributions, message)
        }
        DistributionMethod::Burn
        | DistributionMethod::NextMatchPot
        | DistributionMethod::Cancel => unreachable!(),
    };

    let total_disbursed = complete_and_disburse(
        state,
        guild_id,
        proposal.fund_amount,
        &distributions,
        method,
    )?;
    Ok(ExecutionResult {
        success: true,
        method,
        method_label: method.label(),
        total_disbursed,
        recipient_count: distributions.len(),
        distributions,
        cancelled: false,
        message: empty_message,
    })
}

fn players_for_guild(
    state: &RepositoryState,
    guild_id: GuildId,
) -> impl Iterator<Item = &PlayerSnapshot> {
    state
        .players
        .iter()
        .filter(move |((player_guild, _), _)| *player_guild == guild_id)
        .map(|(_, player)| player)
}

fn players_with_negative_balance(state: &RepositoryState, guild_id: GuildId) -> Vec<Debtor> {
    let mut debtors: Vec<_> = players_for_guild(state, guild_id)
        .filter(|player| player.balance < 0)
        .map(|player| Debtor {
            discord_id: player.discord_id,
            balance: player.balance,
        })
        .collect();
    debtors.sort_by_key(|debtor| (debtor.balance, debtor.discord_id));
    debtors
}

fn stimulus_eligible_players(state: &RepositoryState, guild_id: GuildId) -> Vec<StimulusCandidate> {
    let mut eligible: Vec<_> = players_for_guild(state, guild_id)
        .filter(|player| player.balance >= 0 && player.games_played() > 0)
        .map(|player| StimulusCandidate {
            discord_id: player.discord_id,
            balance: player.balance,
        })
        .collect();
    eligible.sort_by(|left, right| {
        right
            .balance
            .cmp(&left.balance)
            .then_with(|| left.discord_id.cmp(&right.discord_id))
    });
    eligible.into_iter().skip(3).collect()
}

fn lottery_eligible_players(
    state: &RepositoryState,
    guild_id: GuildId,
    activity_days: i64,
) -> Vec<DiscordId> {
    let cutoff = state
        .activity_now
        .saturating_sub(activity_days.saturating_mul(86_400));
    players_for_guild(state, guild_id)
        .filter(|player| player.last_match_at.is_some_and(|played| played >= cutoff))
        .map(|player| player.discord_id)
        .collect()
}

fn top_three_non_debtors(state: &RepositoryState, guild_id: GuildId) -> BTreeSet<DiscordId> {
    let mut non_debtors: Vec<_> = players_for_guild(state, guild_id)
        .filter(|player| player.balance >= 0)
        .map(|player| (player.discord_id, player.balance))
        .collect();
    non_debtors.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    non_debtors
        .into_iter()
        .take(3)
        .map(|(discord_id, _)| discord_id)
        .collect()
}

fn players_by_games_played(state: &RepositoryState, guild_id: GuildId) -> Vec<GamesCandidate> {
    let excluded = top_three_non_debtors(state, guild_id);
    let mut players: Vec<_> = players_for_guild(state, guild_id)
        .filter(|player| player.games_played() > 0 && !excluded.contains(&player.discord_id))
        .map(|player| GamesCandidate {
            discord_id: player.discord_id,
            games_played: player.games_played(),
        })
        .collect();
    players.sort_by(|left, right| {
        right
            .games_played
            .cmp(&left.games_played)
            .then_with(|| left.discord_id.cmp(&right.discord_id))
    });
    players
}

fn richest_player(state: &RepositoryState, guild_id: GuildId) -> Option<RichestCandidate> {
    players_for_guild(state, guild_id)
        .map(|player| RichestCandidate {
            discord_id: player.discord_id,
            balance: player.balance,
        })
        .max_by(|left, right| {
            left.balance
                .cmp(&right.balance)
                .then_with(|| right.discord_id.cmp(&left.discord_id))
        })
}

#[must_use]
pub fn calculate_even_distribution(fund: i64, debtors: &[Debtor]) -> Vec<Distribution> {
    if fund <= 0 || debtors.is_empty() {
        return Vec::new();
    }
    let mut received = vec![0_i64; debtors.len()];
    let debts: Vec<_> = debtors
        .iter()
        .map(|debtor| debtor.balance.saturating_abs())
        .collect();
    let mut remaining = fund;
    let mut unfilled: Vec<usize> = (0..debtors.len()).collect();

    while remaining > 0 && !unfilled.is_empty() {
        let divisor = i64::try_from(unfilled.len()).unwrap_or(i64::MAX);
        let per_player = (remaining / divisor).max(1);
        let mut distributed = 0_i64;
        let mut still_unfilled = Vec::new();
        for &index in &unfilled {
            let need = debts[index].saturating_sub(received[index]);
            if need <= 0 {
                continue;
            }
            let available_this_round = remaining.saturating_sub(distributed);
            let give = per_player.min(need).min(available_this_round);
            if give > 0 {
                received[index] = received[index].saturating_add(give);
                distributed = distributed.saturating_add(give);
                if received[index] < debts[index] {
                    still_unfilled.push(index);
                }
            }
        }
        if distributed == 0 {
            break;
        }
        remaining -= distributed;
        unfilled = still_unfilled;
    }

    debtors
        .iter()
        .zip(received)
        .filter(|(_, amount)| *amount > 0)
        .map(|(debtor, amount)| Distribution::new(debtor.discord_id, amount))
        .collect()
}

#[must_use]
pub fn calculate_proportional_distribution(fund: i64, debtors: &[Debtor]) -> Vec<Distribution> {
    if fund <= 0 || debtors.is_empty() {
        return Vec::new();
    }
    let total_debt: i128 = debtors
        .iter()
        .map(|debtor| i128::from(debtor.balance.saturating_abs()))
        .sum();
    if total_debt == 0 {
        return calculate_even_distribution(fund, debtors);
    }
    let mut sorted = debtors.to_vec();
    sorted.sort_by_key(|debtor| (debtor.balance, debtor.discord_id));
    let mut remaining = fund;
    let last = sorted.len().saturating_sub(1);
    let mut distributions = Vec::new();
    for (index, debtor) in sorted.into_iter().enumerate() {
        let debt = debtor.balance.saturating_abs();
        let amount = if index == last {
            remaining.min(debt)
        } else {
            let numerator = i128::from(debt) * i128::from(fund);
            let share = i64::try_from(numerator / total_debt).unwrap_or(i64::MAX);
            share.min(debt).min(remaining)
        };
        if amount > 0 {
            distributions.push(Distribution::new(debtor.discord_id, amount));
            remaining -= amount;
        }
    }
    distributions
}

#[must_use]
pub fn calculate_neediest_distribution(fund: i64, debtors: &[Debtor]) -> Vec<Distribution> {
    if fund <= 0 {
        return Vec::new();
    }
    debtors
        .iter()
        .min_by_key(|debtor| (debtor.balance, debtor.discord_id))
        .map(|debtor| {
            vec![Distribution::new(
                debtor.discord_id,
                fund.min(debtor.balance.saturating_abs()),
            )]
        })
        .unwrap_or_default()
}

#[must_use]
pub fn calculate_stimulus_distribution(
    fund: i64,
    eligible: &[StimulusCandidate],
) -> Vec<Distribution> {
    if fund <= 0 || eligible.is_empty() {
        return Vec::new();
    }
    let count = i64::try_from(eligible.len()).unwrap_or(i64::MAX);
    let per_player = fund / count;
    let remainder = usize::try_from(fund % count).unwrap_or(0);
    eligible
        .iter()
        .enumerate()
        .map(|(index, player)| {
            let amount = per_player + i64::from(index < remainder);
            Distribution::new(player.discord_id, amount)
        })
        .filter(|distribution| distribution.amount > 0)
        .collect()
}

#[must_use]
pub fn calculate_lottery_distribution(
    fund: i64,
    players: &[DiscordId],
    random_draw: u64,
) -> Vec<Distribution> {
    if fund <= 0 || players.is_empty() {
        return Vec::new();
    }
    let count = u64::try_from(players.len()).unwrap_or(u64::MAX);
    let winner_index = usize::try_from(random_draw % count).unwrap_or(0);
    vec![Distribution::new(players[winner_index], fund)]
}

#[must_use]
pub fn calculate_social_security_distribution(
    fund: i64,
    players: &[GamesCandidate],
) -> Vec<Distribution> {
    if fund <= 0 || players.is_empty() {
        return Vec::new();
    }
    let total_games: u64 = players
        .iter()
        .map(|player| u64::from(player.games_played))
        .sum();
    if total_games == 0 {
        return Vec::new();
    }
    let mut sorted = players.to_vec();
    sorted.sort_by(|left, right| {
        right
            .games_played
            .cmp(&left.games_played)
            .then_with(|| left.discord_id.cmp(&right.discord_id))
    });
    let last = sorted.len().saturating_sub(1);
    let mut remaining = fund;
    let mut distributions = Vec::new();
    for (index, player) in sorted.into_iter().enumerate() {
        let amount = if index == last {
            remaining
        } else {
            let numerator = i128::from(player.games_played) * i128::from(fund);
            i64::try_from(numerator / i128::from(total_games)).unwrap_or(i64::MAX)
        };
        if amount > 0 {
            distributions.push(Distribution::new(player.discord_id, amount));
            remaining -= amount;
        }
    }
    distributions
}

#[must_use]
pub fn calculate_richest_distribution(
    fund: i64,
    richest: Option<RichestCandidate>,
) -> Vec<Distribution> {
    if fund <= 0 {
        return Vec::new();
    }
    richest
        .map(|player| vec![Distribution::new(player.discord_id, fund)])
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "disburse_service/tests.rs"]
mod tests;
