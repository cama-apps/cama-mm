//! Adapter-neutral Mafia application service.
//!
//! The Python bot remains the production Discord controller and SQLite writer.
//! This module ports the game rules behind a transaction port so a later SQLite
//! adapter can share the existing schema without creating a second migration or
//! write authority.  The supplied memory adapter is deliberately transactional:
//! lifecycle, phase claims, bounties, Bookie hits, and payouts all commit under
//! one mutex, making retry and race behavior executable in parity tests.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use thiserror::Error;

pub type GuildId = i64;
pub type PlayerId = i64;
pub type GameId = u64;

pub const MIN_ROSTER: usize = 5;
pub const MAX_ROSTER: usize = 15;
pub const MAX_CYCLES: u8 = 14;
pub const ENTRY_FEE: i64 = 8;
pub const MVP_BONUS: i64 = 20;
pub const MAX_WINNER_PAYOUT: i64 = 50;
pub const BOOKIE_PAYOUT: i64 = MAX_WINNER_PAYOUT;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Phase {
    Setup,
    Night,
    Day,
    Resolved,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Role {
    Mafia,
    Doctor,
    Detective,
    Vigilante,
    Townie,
    Jester,
    Bookie,
}

impl Role {
    #[must_use]
    pub const fn night_action(self) -> Option<ActionType> {
        match self {
            Self::Mafia => Some(ActionType::Kill),
            Self::Doctor => Some(ActionType::Save),
            Self::Detective => Some(ActionType::Investigate),
            Self::Vigilante => Some(ActionType::VigilanteKill),
            Self::Bookie => Some(ActionType::Wager),
            Self::Townie | Self::Jester => None,
        }
    }

    #[must_use]
    pub const fn wins_with_town(self) -> bool {
        matches!(
            self,
            Self::Doctor | Self::Detective | Self::Vigilante | Self::Townie
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ActionType {
    Kill,
    Save,
    Investigate,
    VigilanteKill,
    Vote,
    Wager,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Twist {
    BloodMoon,
    TownHall,
    MemoryFog,
    Plague,
    Resurrection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Winner {
    Town,
    Mafia,
    Jester,
    None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Player {
    pub discord_id: PlayerId,
    pub guild_id: GuildId,
    pub role: Role,
    pub is_godfather: bool,
    pub hero_name: String,
    pub is_alive: bool,
    pub eliminated_phase: Option<Phase>,
}

impl Player {
    #[must_use]
    pub fn new(discord_id: PlayerId, guild_id: GuildId, role: Role) -> Self {
        Self {
            discord_id,
            guild_id,
            role,
            is_godfather: false,
            hero_name: format!("Hero{discord_id}"),
            is_alive: true,
            eliminated_phase: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Game {
    pub game_id: GameId,
    pub guild_id: GuildId,
    pub game_date: String,
    pub phase: Phase,
    pub roster_size: usize,
    pub twist: Option<Twist>,
    pub day_number: u8,
    pub winner: Option<Winner>,
    pub entry_fee: i64,
    pub status: GameStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GameStatus {
    Active,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Action {
    pub game_id: GameId,
    pub actor_id: PlayerId,
    pub target_id: PlayerId,
    pub action_type: ActionType,
    pub phase: Phase,
    pub day_number: u8,
    pub result: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerEntry {
    pub guild_id: GuildId,
    pub account_id: PlayerId,
    pub delta: i64,
    pub source: &'static str,
    pub related_type: &'static str,
    pub related_id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActionResponse {
    pub ok: bool,
    pub error: Option<&'static str>,
    pub result: Option<&'static str>,
    pub changed: bool,
}

impl ActionResponse {
    const fn accepted() -> Self {
        Self {
            ok: true,
            error: None,
            result: None,
            changed: false,
        }
    }

    const fn rejected(error: &'static str) -> Self {
        Self {
            ok: false,
            error: Some(error),
            result: None,
            changed: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Death {
    pub discord_id: PlayerId,
    pub source: &'static str,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NightResolution {
    pub resolved: bool,
    pub reason: Option<&'static str>,
    pub killed: Vec<Death>,
    pub save_blocked: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BountyResolution {
    pub paid: BTreeMap<PlayerId, i64>,
    pub reward: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DayResolution {
    pub resolved: bool,
    pub reason: Option<&'static str>,
    pub continued: bool,
    pub winner: Option<Winner>,
    pub lynched_id: Option<PlayerId>,
    pub mvp_id: Option<PlayerId>,
    pub mvp_bonus: i64,
    pub payout_per_winner: i64,
    pub pot_total: i64,
    pub winning_ids: Vec<PlayerId>,
    pub bookie_id: Option<PlayerId>,
    pub bookie_payout: i64,
    pub nonprofit_overflow: i64,
    pub bankruptcy_penalties: BTreeMap<PlayerId, i64>,
    pub vanity_taxes: BTreeMap<PlayerId, i64>,
    pub bounty: BountyResolution,
    pub cap_forced: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicStatus {
    pub active: bool,
    pub phase: Option<Phase>,
    pub alive_count: usize,
    pub roster_size: usize,
    pub voted_count: usize,
    pub alive_voters: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ally {
    pub discord_id: PlayerId,
    pub is_godfather: bool,
    pub hero_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoleView {
    pub role: Role,
    pub is_godfather: bool,
    pub hero_name: String,
    pub is_alive: bool,
    pub phase: Phase,
    pub allies: Vec<Ally>,
    pub investigations: Vec<(PlayerId, String)>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ServiceError {
    #[error("the durable phase transition committed before the adapter failed")]
    PostCommitFault,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Bounty {
    game_id: GameId,
    day_number: u8,
    target_id: PlayerId,
    contributor_id: PlayerId,
    amount: i64,
}

/// Transaction state shared by repository adapters.
#[derive(Debug, Default)]
pub struct MafiaState {
    registrations: BTreeSet<(GuildId, PlayerId)>,
    balances: BTreeMap<(GuildId, PlayerId), i64>,
    signups: BTreeMap<GuildId, Vec<PlayerId>>,
    optouts: BTreeSet<(GuildId, PlayerId)>,
    games: BTreeMap<GameId, Game>,
    players: BTreeMap<GameId, Vec<Player>>,
    actions: Vec<Action>,
    bounties: Vec<Bounty>,
    nonprofit: BTreeMap<GuildId, i64>,
    bankruptcy_players: BTreeSet<(GuildId, PlayerId)>,
    vanity_taxable: BTreeSet<(GuildId, PlayerId)>,
    ledger: Vec<LedgerEntry>,
    next_game_id: GameId,
    lose_next_night_claim: bool,
    lose_next_day_claim: bool,
    post_commit_fault: bool,
    detective_backfills: usize,
}

impl MafiaState {
    fn active_game_id(&self, guild_id: GuildId) -> Option<GameId> {
        self.games
            .values()
            .find(|game| {
                game.guild_id == guild_id
                    && game.phase != Phase::Resolved
                    && game.status == GameStatus::Active
            })
            .map(|game| game.game_id)
    }
}

/// Atomic state boundary. A SQLite implementation should map `transaction` to
/// the same immediate transaction used by the Python repository.
pub trait MafiaRepositoryPort: Clone + Send + Sync + 'static {
    fn read<T>(&self, operation: impl FnOnce(&MafiaState) -> T) -> T;
    fn transaction<T>(&self, operation: impl FnOnce(&mut MafiaState) -> T) -> T;
}

#[derive(Clone, Debug, Default)]
pub struct MemoryMafiaRepository {
    state: Arc<Mutex<MafiaState>>,
}

impl MafiaRepositoryPort for MemoryMafiaRepository {
    fn read<T>(&self, operation: impl FnOnce(&MafiaState) -> T) -> T {
        let guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        operation(&guard)
    }

    fn transaction<T>(&self, operation: impl FnOnce(&mut MafiaState) -> T) -> T {
        let mut guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        operation(&mut guard)
    }
}

impl MemoryMafiaRepository {
    pub fn register(&self, guild_id: GuildId, player_id: PlayerId, balance: i64) {
        self.transaction(|state| {
            state.registrations.insert((guild_id, player_id));
            state.balances.insert((guild_id, player_id), balance);
        });
    }

    pub fn signup(&self, guild_id: GuildId, player_id: PlayerId) {
        self.transaction(|state| {
            let signups = state.signups.entry(guild_id).or_default();
            if !signups.contains(&player_id) {
                signups.push(player_id);
            }
        });
    }

    #[must_use]
    pub fn signups(&self, guild_id: GuildId) -> Vec<PlayerId> {
        self.read(|state| state.signups.get(&guild_id).cloned().unwrap_or_default())
    }

    #[must_use]
    pub fn balance(&self, guild_id: GuildId, player_id: PlayerId) -> i64 {
        self.read(|state| {
            state
                .balances
                .get(&(guild_id, player_id))
                .copied()
                .unwrap_or(0)
        })
    }

    pub fn set_balance(&self, guild_id: GuildId, player_id: PlayerId, balance: i64) {
        self.transaction(|state| {
            state.balances.insert((guild_id, player_id), balance);
        });
    }

    #[must_use]
    pub fn game(&self, game_id: GameId) -> Option<Game> {
        self.read(|state| state.games.get(&game_id).cloned())
    }

    #[must_use]
    pub fn active_game(&self, guild_id: GuildId) -> Option<Game> {
        self.read(|state| {
            state
                .active_game_id(guild_id)
                .and_then(|id| state.games.get(&id).cloned())
        })
    }

    #[must_use]
    pub fn players(&self, game_id: GameId) -> Vec<Player> {
        self.read(|state| state.players.get(&game_id).cloned().unwrap_or_default())
    }

    #[must_use]
    pub fn actions(&self, game_id: GameId) -> Vec<Action> {
        self.read(|state| {
            state
                .actions
                .iter()
                .filter(|action| action.game_id == game_id)
                .cloned()
                .collect()
        })
    }

    #[must_use]
    pub fn nonprofit_fund(&self, guild_id: GuildId) -> i64 {
        self.read(|state| state.nonprofit.get(&guild_id).copied().unwrap_or(0))
    }

    #[must_use]
    pub fn ledger(&self) -> Vec<LedgerEntry> {
        self.read(|state| state.ledger.clone())
    }

    pub fn seed_game(&self, guild_id: GuildId, phase: Phase, players: Vec<Player>) -> GameId {
        self.transaction(|state| {
            state.next_game_id += 1;
            let game_id = state.next_game_id;
            let game = Game {
                game_id,
                guild_id,
                game_date: "2026-04-24".to_owned(),
                phase,
                roster_size: players.len(),
                twist: None,
                day_number: 1,
                winner: None,
                entry_fee: ENTRY_FEE,
                status: GameStatus::Active,
            };
            state.games.insert(game_id, game);
            state.players.insert(game_id, players);
            game_id
        })
    }

    pub fn set_phase(&self, game_id: GameId, phase: Phase) {
        self.transaction(|state| {
            if let Some(game) = state.games.get_mut(&game_id) {
                game.phase = phase;
            }
        });
    }

    pub fn set_twist(&self, game_id: GameId, twist: Option<Twist>) {
        self.transaction(|state| {
            if let Some(game) = state.games.get_mut(&game_id) {
                game.twist = twist;
            }
        });
    }

    pub fn set_day_number(&self, game_id: GameId, day_number: u8) {
        self.transaction(|state| {
            if let Some(game) = state.games.get_mut(&game_id) {
                game.day_number = day_number;
            }
        });
    }

    pub fn mark_bankrupt(&self, guild_id: GuildId, player_id: PlayerId) {
        self.transaction(|state| {
            state.bankruptcy_players.insert((guild_id, player_id));
        });
    }

    pub fn mark_vanity_taxable(&self, guild_id: GuildId, player_id: PlayerId) {
        self.transaction(|state| {
            state.vanity_taxable.insert((guild_id, player_id));
        });
    }

    pub fn lose_next_night_claim(&self) {
        self.transaction(|state| state.lose_next_night_claim = true);
    }

    pub fn lose_next_day_claim(&self) {
        self.transaction(|state| state.lose_next_day_claim = true);
    }

    pub fn inject_post_commit_fault(&self) {
        self.transaction(|state| state.post_commit_fault = true);
    }

    #[must_use]
    pub fn detective_backfills(&self) -> usize {
        self.read(|state| state.detective_backfills)
    }

    pub fn try_reopen_for_finalize(&self, game_id: GameId) -> bool {
        self.transaction(|state| {
            let Some(game) = state.games.get_mut(&game_id) else {
                return false;
            };
            if game.phase == Phase::Resolved {
                return false;
            }
            game.phase = Phase::Day;
            true
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoleCounts {
    pub mafia: usize,
    pub doctor: usize,
    pub detective: usize,
    pub vigilante: usize,
}

#[must_use]
pub const fn role_counts(roster_size: usize) -> RoleCounts {
    match roster_size {
        5 | 6 => RoleCounts {
            mafia: 1,
            doctor: 1,
            detective: 1,
            vigilante: 0,
        },
        7..=9 => RoleCounts {
            mafia: 2,
            doctor: 1,
            detective: 1,
            vigilante: 0,
        },
        10..=12 => RoleCounts {
            mafia: 3,
            doctor: 1,
            detective: 1,
            vigilante: 1,
        },
        13..=15 => RoleCounts {
            mafia: 4,
            doctor: 1,
            detective: 1,
            vigilante: 1,
        },
        _ => RoleCounts {
            mafia: 0,
            doctor: 0,
            detective: 0,
            vigilante: 0,
        },
    }
}

#[must_use]
pub fn pot_for_roster(roster_size: usize) -> i64 {
    i64::try_from(roster_size).unwrap_or(i64::MAX / ENTRY_FEE) * ENTRY_FEE
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayoutPlan {
    pub deltas: BTreeMap<PlayerId, i64>,
    pub payout_per_winner: i64,
    pub nonprofit_overflow: i64,
}

#[must_use]
pub fn compute_payouts(
    pot_total: i64,
    winning_ids: &[PlayerId],
    mvp_id: Option<PlayerId>,
    bookie_id: Option<PlayerId>,
) -> PayoutPlan {
    let mut deltas = BTreeMap::new();
    let mut faction_pot = pot_total;
    if let Some(bookie) = bookie_id {
        let take = BOOKIE_PAYOUT.min(pot_total);
        deltas.insert(bookie, take);
        faction_pot -= take;
    }

    let mut base = 0;
    if !winning_ids.is_empty() {
        let mvp_wins = mvp_id.is_some_and(|id| winning_ids.contains(&id));
        let mvp_share = if mvp_wins {
            MVP_BONUS.min(faction_pot)
        } else {
            0
        };
        let remainder = faction_pot - mvp_share;
        base = remainder / i64::try_from(winning_ids.len()).unwrap_or(1);
        let dust = remainder - base * i64::try_from(winning_ids.len()).unwrap_or(1);
        for &winner in winning_ids {
            deltas.insert(winner, base);
        }
        if let Some(mvp) = mvp_id.filter(|id| winning_ids.contains(id)) {
            *deltas.entry(mvp).or_default() += mvp_share + dust;
        } else if let Some(first) = winning_ids.first() {
            *deltas.entry(*first).or_default() += dust;
        }
    }

    for amount in deltas.values_mut() {
        *amount = (*amount).min(MAX_WINNER_PAYOUT);
    }
    let distributed: i64 = deltas.values().sum();
    PayoutPlan {
        deltas,
        payout_per_winner: base.min(MAX_WINNER_PAYOUT),
        nonprofit_overflow: pot_total - distributed,
    }
}

#[derive(Clone)]
pub struct MafiaService<R: MafiaRepositoryPort = MemoryMafiaRepository> {
    repo: R,
    entropy: Arc<AtomicU64>,
    pub max_debt: i64,
    pub bankruptcy_penalty_rate_basis_points: u16,
    pub vanity_tax_basis_points: u16,
}

impl<R: MafiaRepositoryPort> MafiaService<R> {
    #[must_use]
    pub fn new(repo: R, seed: u64) -> Self {
        Self {
            repo,
            entropy: Arc::new(AtomicU64::new(seed.max(1))),
            max_debt: 500,
            bankruptcy_penalty_rate_basis_points: 0,
            vanity_tax_basis_points: 0,
        }
    }

    #[must_use]
    pub fn repository(&self) -> R {
        self.repo.clone()
    }

    fn next_random(&self) -> u64 {
        let mut current = self.entropy.load(Ordering::Relaxed);
        loop {
            let mut next = current;
            next ^= next << 13;
            next ^= next >> 7;
            next ^= next << 17;
            match self.entropy.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(observed) => current = observed,
            }
        }
    }

    #[must_use]
    pub fn assign_roles(&self, guild_id: GuildId, player_ids: &[PlayerId]) -> Vec<Player> {
        let jester = self.next_random().is_multiple_of(5);
        let bookie = self.next_random().is_multiple_of(5);
        self.assign_roles_with_wildcards(guild_id, player_ids, jester, bookie)
    }

    #[must_use]
    pub fn assign_roles_with_wildcards(
        &self,
        guild_id: GuildId,
        player_ids: &[PlayerId],
        add_jester: bool,
        add_bookie: bool,
    ) -> Vec<Player> {
        let counts = role_counts(player_ids.len());
        let mut order = player_ids.to_vec();
        if !order.is_empty() {
            let shift = usize::try_from(self.next_random()).unwrap_or(0) % order.len();
            order.rotate_left(shift);
        }
        let mut assignments = BTreeMap::new();
        let mut index = 0;
        for (role, count) in [
            (Role::Mafia, counts.mafia),
            (Role::Doctor, counts.doctor),
            (Role::Detective, counts.detective),
            (Role::Vigilante, counts.vigilante),
        ] {
            for _ in 0..count {
                if let Some(&player_id) = order.get(index) {
                    assignments.insert(player_id, role);
                }
                index += 1;
            }
        }
        for &player_id in order.get(index..).unwrap_or_default() {
            assignments.insert(player_id, Role::Townie);
        }

        let mut townies: Vec<_> = assignments
            .iter()
            .filter_map(|(&id, &role)| (role == Role::Townie).then_some(id))
            .collect();
        if add_jester && townies.len() >= 2 {
            let id = townies.remove(0);
            assignments.insert(id, Role::Jester);
        }
        if add_bookie && townies.len() >= 2 {
            let id = townies[0];
            assignments.insert(id, Role::Bookie);
        }

        let godfather = order
            .iter()
            .copied()
            .find(|id| assignments.get(id) == Some(&Role::Mafia));
        player_ids
            .iter()
            .enumerate()
            .map(|(hero_index, &player_id)| Player {
                discord_id: player_id,
                guild_id,
                role: assignments.get(&player_id).copied().unwrap_or(Role::Townie),
                is_godfather: Some(player_id) == godfather,
                hero_name: format!("Hero{hero_index}"),
                is_alive: true,
                eliminated_phase: None,
            })
            .collect()
    }

    pub fn join(&self, guild_id: GuildId, player_id: PlayerId) -> ActionResponse {
        self.repo.transaction(|state| {
            if !state.registrations.contains(&(guild_id, player_id)) {
                return ActionResponse::rejected("not_registered");
            }
            let signups = state.signups.entry(guild_id).or_default();
            if !signups.contains(&player_id) {
                signups.push(player_id);
            }
            state.optouts.remove(&(guild_id, player_id));
            ActionResponse::accepted()
        })
    }

    pub fn set_optout(&self, guild_id: GuildId, player_id: PlayerId, opted_out: bool) {
        self.repo.transaction(|state| {
            if opted_out {
                state.optouts.insert((guild_id, player_id));
                if let Some(signups) = state.signups.get_mut(&guild_id) {
                    signups.retain(|id| *id != player_id);
                }
            } else {
                state.optouts.remove(&(guild_id, player_id));
            }
        });
    }

    pub fn start_game(&self, guild_id: GuildId, game_date: &str) -> Option<Game> {
        self.repo.transaction(|state| {
            if let Some(game_id) = state.active_game_id(guild_id) {
                return state.games.get(&game_id).cloned();
            }
            let eligible: Vec<_> = state
                .signups
                .get(&guild_id)
                .into_iter()
                .flatten()
                .copied()
                .filter(|player_id| {
                    state.registrations.contains(&(guild_id, *player_id))
                        && !state.optouts.contains(&(guild_id, *player_id))
                        && state
                            .balances
                            .get(&(guild_id, *player_id))
                            .copied()
                            .unwrap_or(0)
                            - ENTRY_FEE
                            >= -self.max_debt
                })
                .take(MAX_ROSTER)
                .collect();
            if eligible.len() < MIN_ROSTER {
                return None;
            }
            state.next_game_id += 1;
            let game_id = state.next_game_id;
            let players = self.assign_roles(guild_id, &eligible);
            for &player_id in &eligible {
                *state.balances.entry((guild_id, player_id)).or_default() -= ENTRY_FEE;
            }
            if let Some(signups) = state.signups.get_mut(&guild_id) {
                signups.retain(|id| !eligible.contains(id));
            }
            let game = Game {
                game_id,
                guild_id,
                game_date: game_date.to_owned(),
                phase: Phase::Night,
                roster_size: eligible.len(),
                twist: None,
                day_number: 1,
                winner: None,
                entry_fee: ENTRY_FEE,
                status: GameStatus::Active,
            };
            state.players.insert(game_id, players);
            state.games.insert(game_id, game.clone());
            Some(game)
        })
    }

    fn find_player(players: &[Player], player_id: PlayerId) -> Option<&Player> {
        players.iter().find(|player| player.discord_id == player_id)
    }

    pub fn submit_night_action(
        &self,
        guild_id: GuildId,
        actor_id: PlayerId,
        target_id: PlayerId,
        action_type: ActionType,
    ) -> ActionResponse {
        self.repo.transaction(|state| {
            let Some(game_id) = state.active_game_id(guild_id) else {
                return ActionResponse::rejected("Night phase is not active.");
            };
            let Some(game) = state.games.get(&game_id) else {
                return ActionResponse::rejected("Night phase is not active.");
            };
            if game.phase != Phase::Night {
                return ActionResponse::rejected("Night phase is not active.");
            }
            let day_number = game.day_number;
            let twist = game.twist;
            let Some(players) = state.players.get(&game_id) else {
                return ActionResponse::rejected("You're not in today's game.");
            };
            let Some(actor) = Self::find_player(players, actor_id) else {
                return ActionResponse::rejected("You're not in today's game.");
            };
            if !actor.is_alive {
                return ActionResponse::rejected("You're dead. Rest in peace.");
            }
            if actor.role.night_action() != Some(action_type) {
                return ActionResponse::rejected("Your role can't perform that action.");
            }
            let Some(target) = Self::find_player(players, target_id) else {
                return ActionResponse::rejected("Target is not a living player.");
            };
            if !target.is_alive {
                return ActionResponse::rejected("Target is not a living player.");
            }
            if action_type == ActionType::Kill && target.role == Role::Mafia {
                return ActionResponse::rejected("Mafia don't kill their own.");
            }
            if action_type == ActionType::Investigate && target_id == actor_id {
                return ActionResponse::rejected("You can't investigate yourself.");
            }
            let prior = state.actions.iter().find(|action| {
                action.game_id == game_id
                    && action.actor_id == actor_id
                    && action.action_type == action_type
                    && (action_type == ActionType::VigilanteKill || action.day_number == day_number)
            });
            if let Some(prior) = prior {
                if action_type == ActionType::Investigate && prior.target_id == target_id {
                    return ActionResponse {
                        ok: true,
                        result: prior
                            .result
                            .as_deref()
                            .map(|value| if value == "Mafia" { "Mafia" } else { "Town" }),
                        ..ActionResponse::default()
                    };
                }
                if action_type == ActionType::Investigate {
                    return ActionResponse::rejected("read_locked");
                }
                if action_type == ActionType::VigilanteKill {
                    return ActionResponse::rejected("one_shot_used");
                }
            }
            let result = if action_type == ActionType::Investigate {
                Some(
                    if twist == Some(Twist::MemoryFog)
                        || target.is_godfather
                        || target.role != Role::Mafia
                    {
                        "Town"
                    } else {
                        "Mafia"
                    },
                )
            } else {
                None
            };
            state.actions.push(Action {
                game_id,
                actor_id,
                target_id,
                action_type,
                phase: Phase::Night,
                day_number,
                result: result.map(str::to_owned),
            });
            ActionResponse {
                ok: true,
                result,
                ..ActionResponse::default()
            }
        })
    }

    pub fn submit_day_vote(
        &self,
        guild_id: GuildId,
        voter_id: PlayerId,
        target_id: PlayerId,
    ) -> ActionResponse {
        self.repo.transaction(|state| {
            let Some(game_id) = state.active_game_id(guild_id) else {
                return ActionResponse::rejected("Day phase is not active.");
            };
            let Some(game) = state.games.get(&game_id) else {
                return ActionResponse::rejected("Day phase is not active.");
            };
            if game.phase != Phase::Day {
                return ActionResponse::rejected("Day phase is not active.");
            }
            let day_number = game.day_number;
            let Some(players) = state.players.get(&game_id) else {
                return ActionResponse::rejected("You're not in today's game.");
            };
            if !Self::find_player(players, voter_id).is_some_and(|player| player.is_alive) {
                return ActionResponse::rejected("voter_not_alive");
            }
            if !Self::find_player(players, target_id).is_some_and(|player| player.is_alive) {
                return ActionResponse::rejected("target_not_alive");
            }
            if let Some(prior) = state.actions.iter_mut().find(|action| {
                action.game_id == game_id
                    && action.actor_id == voter_id
                    && action.action_type == ActionType::Vote
                    && action.day_number == day_number
            }) {
                let changed = prior.target_id != target_id;
                prior.target_id = target_id;
                return ActionResponse {
                    ok: true,
                    changed,
                    ..ActionResponse::default()
                };
            }
            state.actions.push(Action {
                game_id,
                actor_id: voter_id,
                target_id,
                action_type: ActionType::Vote,
                phase: Phase::Day,
                day_number,
                result: None,
            });
            ActionResponse::accepted()
        })
    }

    pub fn submit_bounty(
        &self,
        guild_id: GuildId,
        contributor_id: PlayerId,
        target_id: PlayerId,
    ) -> ActionResponse {
        self.repo.transaction(|state| {
            let Some(game_id) = state.active_game_id(guild_id) else {
                return ActionResponse::rejected("Bounties can only be placed during the day.");
            };
            let game = &state.games[&game_id];
            if game.phase != Phase::Day {
                return ActionResponse::rejected("Bounties can only be placed during the day.");
            }
            let day_number = game.day_number;
            let players = &state.players[&game_id];
            if !Self::find_player(players, contributor_id).is_some_and(|player| player.is_alive) {
                return ActionResponse::rejected("Only living players can place bounties.");
            }
            if contributor_id == target_id {
                return ActionResponse::rejected("You can't put a bounty on yourself.");
            }
            if !Self::find_player(players, target_id).is_some_and(|player| player.is_alive) {
                return ActionResponse::rejected("Target is not a living player.");
            }
            if state.bounties.iter().any(|bounty| {
                bounty.game_id == game_id
                    && bounty.day_number == day_number
                    && bounty.target_id == target_id
                    && bounty.contributor_id == contributor_id
            }) {
                return ActionResponse::rejected("already_staked");
            }
            let balance = state
                .balances
                .entry((guild_id, contributor_id))
                .or_default();
            if *balance - 1 < -self.max_debt {
                return ActionResponse::rejected("insufficient_funds");
            }
            *balance -= 1;
            *state.nonprofit.entry(guild_id).or_default() += 1;
            state.bounties.push(Bounty {
                game_id,
                day_number,
                target_id,
                contributor_id,
                amount: 1,
            });
            ActionResponse::accepted()
        })
    }

    #[must_use]
    pub fn players_needing_night_action(&self, game: &Game) -> Vec<PlayerId> {
        self.repo.read(|state| {
            state
                .players
                .get(&game.game_id)
                .into_iter()
                .flatten()
                .filter(|player| {
                    player.is_alive
                        && !state.optouts.contains(&(game.guild_id, player.discord_id))
                        && player.role.night_action().is_some()
                })
                .filter(|player| {
                    let expected = player.role.night_action().unwrap_or(ActionType::Vote);
                    !state.actions.iter().any(|action| {
                        action.game_id == game.game_id
                            && action.day_number == game.day_number
                            && action.actor_id == player.discord_id
                            && action.action_type == expected
                    })
                })
                .map(|player| player.discord_id)
                .collect()
        })
    }

    #[must_use]
    pub fn players_needing_day_vote(&self, game: &Game) -> Vec<PlayerId> {
        self.repo.read(|state| {
            state
                .players
                .get(&game.game_id)
                .into_iter()
                .flatten()
                .filter(|player| {
                    player.is_alive && !state.optouts.contains(&(game.guild_id, player.discord_id))
                })
                .filter(|player| {
                    !state.actions.iter().any(|action| {
                        action.game_id == game.game_id
                            && action.day_number == game.day_number
                            && action.actor_id == player.discord_id
                            && action.action_type == ActionType::Vote
                    })
                })
                .map(|player| player.discord_id)
                .collect()
        })
    }

    #[must_use]
    pub fn night_ready(&self, game: &Game) -> bool {
        self.players_needing_night_action(game).is_empty()
    }

    #[must_use]
    pub fn day_ready(&self, game: &Game) -> bool {
        self.players_needing_day_vote(game).is_empty()
    }

    pub fn resolve_night(&self, guild_id: GuildId) -> NightResolution {
        self.repo.transaction(|state| {
            let Some(game_id) = state.active_game_id(guild_id) else {
                return NightResolution {
                    reason: Some("no_active_night"),
                    ..NightResolution::default()
                };
            };
            let game = state.games[&game_id].clone();
            if game.phase != Phase::Night {
                return NightResolution {
                    reason: Some("no_active_night"),
                    ..NightResolution::default()
                };
            }
            if state.lose_next_night_claim {
                state.lose_next_night_claim = false;
                return NightResolution {
                    reason: Some("night_already_resolved"),
                    ..NightResolution::default()
                };
            }
            let players = state.players[&game_id].clone();
            let alive: BTreeSet<_> = players
                .iter()
                .filter(|player| player.is_alive)
                .map(|player| player.discord_id)
                .collect();
            let mafia: BTreeSet<_> = players
                .iter()
                .filter(|player| player.is_alive && player.role == Role::Mafia)
                .map(|player| player.discord_id)
                .collect();
            let actions: Vec<_> = state
                .actions
                .iter()
                .filter(|action| action.game_id == game_id && action.day_number == game.day_number)
                .cloned()
                .collect();
            let mut kill_counts = BTreeMap::new();
            for action in actions
                .iter()
                .filter(|action| action.action_type == ActionType::Kill)
            {
                if alive.contains(&action.target_id) && !mafia.contains(&action.target_id) {
                    *kill_counts.entry(action.target_id).or_insert(0_usize) += 1;
                }
            }
            let target_count = usize::from(game.twist == Some(Twist::BloodMoon)) + 1;
            let mut ordered: Vec<_> = kill_counts.into_iter().collect();
            ordered.sort_by_key(|(id, count)| (std::cmp::Reverse(*count), *id));
            let mut mafia_targets: Vec<_> = ordered
                .into_iter()
                .map(|(id, _)| id)
                .take(target_count)
                .collect();
            if mafia_targets.len() < target_count {
                for candidate in alive.difference(&mafia) {
                    if !mafia_targets.contains(candidate) {
                        mafia_targets.push(*candidate);
                        if mafia_targets.len() == target_count {
                            break;
                        }
                    }
                }
            }
            let save_target = actions
                .iter()
                .find(|action| action.action_type == ActionType::Save)
                .map(|action| action.target_id);
            let save_blocked = save_target.is_some_and(|id| mafia_targets.contains(&id));
            if let Some(saved) = save_target {
                mafia_targets.retain(|id| *id != saved);
            }
            let vig_target = actions
                .iter()
                .find(|action| action.action_type == ActionType::VigilanteKill)
                .map(|action| action.target_id)
                .filter(|id| alive.contains(id) && !mafia_targets.contains(id));
            let mut deaths: Vec<_> = mafia_targets
                .iter()
                .map(|id| Death {
                    discord_id: *id,
                    source: "mafia",
                })
                .collect();
            if let Some(id) = vig_target {
                deaths.push(Death {
                    discord_id: id,
                    source: "vigilante",
                });
            }
            if game.twist == Some(Twist::Plague)
                && let Some(id) = alive
                    .iter()
                    .find(|id| !deaths.iter().any(|death| death.discord_id == **id))
            {
                deaths.push(Death {
                    discord_id: *id,
                    source: "plague",
                });
            }
            if let Some(roster) = state.players.get_mut(&game_id) {
                for player in roster {
                    if deaths
                        .iter()
                        .any(|death| death.discord_id == player.discord_id)
                    {
                        player.is_alive = false;
                        player.eliminated_phase = Some(Phase::Night);
                    }
                }
            }
            if let Some(active) = state.games.get_mut(&game_id) {
                active.phase = Phase::Day;
            }
            for action in state.actions.iter_mut().filter(|action| {
                action.game_id == game_id
                    && action.day_number == game.day_number
                    && action.action_type == ActionType::Investigate
                    && action.result.is_none()
            }) {
                let result = players
                    .iter()
                    .find(|player| player.discord_id == action.target_id)
                    .map_or("Town", |target| {
                        if target.role == Role::Mafia && !target.is_godfather {
                            "Mafia"
                        } else {
                            "Town"
                        }
                    });
                action.result = Some(result.to_owned());
                state.detective_backfills += 1;
            }
            NightResolution {
                resolved: true,
                reason: None,
                killed: deaths,
                save_blocked,
            }
        })
    }

    fn valid_votes(state: &MafiaState, game: &Game, players: &[Player]) -> Vec<Action> {
        let alive: BTreeSet<_> = players
            .iter()
            .filter(|player| player.is_alive)
            .map(|player| player.discord_id)
            .collect();
        state
            .actions
            .iter()
            .filter(|action| {
                action.game_id == game.game_id
                    && action.day_number == game.day_number
                    && action.action_type == ActionType::Vote
                    && alive.contains(&action.actor_id)
                    && alive.contains(&action.target_id)
            })
            .cloned()
            .collect()
    }

    fn tally_lynch(votes: &[Action]) -> Option<PlayerId> {
        let mut counts = BTreeMap::new();
        for vote in votes {
            *counts.entry(vote.target_id).or_insert(0_usize) += 1;
        }
        let maximum = counts.values().copied().max()?;
        let leaders: Vec<_> = counts
            .into_iter()
            .filter_map(|(id, count)| (count == maximum).then_some(id))
            .collect();
        (leaders.len() == 1).then_some(leaders[0])
    }

    fn winning_ids(players: &[Player], winner: Winner) -> Vec<PlayerId> {
        players
            .iter()
            .filter(|player| match winner {
                Winner::Town => player.role.wins_with_town(),
                Winner::Mafia => player.role == Role::Mafia,
                Winner::Jester => player.role == Role::Jester,
                Winner::None => false,
            })
            .map(|player| player.discord_id)
            .collect()
    }

    fn settle_bounties(
        state: &mut MafiaState,
        game: &Game,
        lynched_id: Option<PlayerId>,
        lynched_was_mafia: bool,
        alive_count: usize,
    ) -> BountyResolution {
        if !lynched_was_mafia {
            return BountyResolution::default();
        }
        let Some(target_id) = lynched_id else {
            return BountyResolution::default();
        };
        let matching: Vec<_> = state
            .bounties
            .iter()
            .filter(|bounty| {
                bounty.game_id == game.game_id
                    && bounty.day_number == game.day_number
                    && bounty.target_id == target_id
            })
            .cloned()
            .collect();
        if matching.is_empty() {
            return BountyResolution::default();
        }
        let staked: i64 = matching.iter().map(|bounty| bounty.amount).sum();
        let available = state.nonprofit.get(&game.guild_id).copied().unwrap_or(0);
        let reward = staked
            .min(available)
            .min(i64::try_from(alive_count).unwrap_or(i64::MAX));
        if reward <= 0 {
            return BountyResolution::default();
        }
        *state.nonprofit.entry(game.guild_id).or_default() -= reward;
        let base = reward / i64::try_from(matching.len()).unwrap_or(1);
        let dust = reward - base * i64::try_from(matching.len()).unwrap_or(1);
        let mut paid = BTreeMap::new();
        for (index, bounty) in matching.iter().enumerate() {
            let amount = base + if index == 0 { dust } else { 0 };
            if amount > 0 {
                *state
                    .balances
                    .entry((game.guild_id, bounty.contributor_id))
                    .or_default() += amount;
                paid.insert(bounty.contributor_id, amount);
            }
        }
        BountyResolution { paid, reward }
    }

    pub fn resolve_day(&self, guild_id: GuildId) -> Result<DayResolution, ServiceError> {
        self.repo.transaction(|state| {
            let Some(game_id) = state.active_game_id(guild_id) else {
                return Ok(DayResolution {
                    reason: Some("no_active_day"),
                    ..DayResolution::default()
                });
            };
            let game = state.games[&game_id].clone();
            if game.phase != Phase::Day {
                return Ok(DayResolution {
                    reason: Some("no_active_day"),
                    ..DayResolution::default()
                });
            }
            let players = state.players[&game_id].clone();
            let votes = Self::valid_votes(state, &game, &players);
            let lynched_id = if game.twist == Some(Twist::TownHall) && game.day_number == 1 {
                None
            } else {
                Self::tally_lynch(&votes)
            };
            let lynched_role = lynched_id.and_then(|id| {
                players
                    .iter()
                    .find(|player| player.discord_id == id)
                    .map(|player| player.role)
            });
            let post_players: Vec<_> = players
                .iter()
                .cloned()
                .map(|mut player| {
                    if Some(player.discord_id) == lynched_id {
                        player.is_alive = false;
                        player.eliminated_phase = Some(Phase::Day);
                    }
                    player
                })
                .collect();
            let alive_mafia = post_players
                .iter()
                .filter(|player| player.is_alive && player.role == Role::Mafia)
                .count();
            let alive_non_mafia = post_players
                .iter()
                .filter(|player| player.is_alive && player.role != Role::Mafia)
                .count();
            let cap_reached = game.day_number >= MAX_CYCLES;
            let winner = if lynched_role == Some(Role::Jester) {
                Some(Winner::Jester)
            } else if alive_mafia == 0 {
                Some(Winner::Town)
            } else if alive_mafia >= alive_non_mafia {
                Some(Winner::Mafia)
            } else if cap_reached {
                Some(if alive_non_mafia > alive_mafia {
                    Winner::Town
                } else {
                    Winner::Mafia
                })
            } else {
                None
            };

            if state.lose_next_day_claim {
                state.lose_next_day_claim = false;
                return Ok(DayResolution {
                    reason: Some("day_already_resolved"),
                    ..DayResolution::default()
                });
            }

            state.players.insert(game_id, post_players.clone());
            let lynched_was_mafia = lynched_role == Some(Role::Mafia);
            for wager in state.actions.iter_mut().filter(|action| {
                action.game_id == game_id
                    && action.day_number == game.day_number
                    && action.action_type == ActionType::Wager
                    && Some(action.target_id) == lynched_id
            }) {
                wager.result = Some("HIT".to_owned());
            }
            let bounty = Self::settle_bounties(
                state,
                &game,
                lynched_id,
                lynched_was_mafia,
                alive_mafia + alive_non_mafia,
            );

            let mut resolution = if let Some(winner) = winner {
                let winning_ids = Self::winning_ids(&post_players, winner);
                let mvp_id = if winner == Winner::Jester {
                    winning_ids.first().copied()
                } else if winner == Winner::Town && lynched_id.is_some() {
                    votes.iter().find_map(|vote| {
                        (Some(vote.target_id) == lynched_id
                            && players.iter().any(|player| {
                                player.discord_id == vote.actor_id && player.role != Role::Mafia
                            }))
                        .then_some(vote.actor_id)
                    })
                } else {
                    winning_ids.first().copied()
                };
                let bookie_id = post_players
                    .iter()
                    .find(|player| {
                        player.is_alive
                            && player.role == Role::Bookie
                            && state.actions.iter().any(|action| {
                                action.game_id == game_id
                                    && action.actor_id == player.discord_id
                                    && action.action_type == ActionType::Wager
                                    && action.result.as_deref() == Some("HIT")
                            })
                    })
                    .map(|player| player.discord_id);
                let pot_total = pot_for_roster(game.roster_size);
                let payout = compute_payouts(pot_total, &winning_ids, mvp_id, bookie_id);
                let mut penalties = BTreeMap::new();
                let mut vanity_taxes = BTreeMap::new();
                for (&player_id, &gross) in &payout.deltas {
                    let profit = (gross - game.entry_fee).max(0);
                    let penalty = if state
                        .bankruptcy_players
                        .contains(&(game.guild_id, player_id))
                    {
                        profit * i64::from(self.bankruptcy_penalty_rate_basis_points) / 10_000
                    } else {
                        0
                    };
                    let vanity_tax = if state.vanity_taxable.contains(&(game.guild_id, player_id)) {
                        profit * i64::from(self.vanity_tax_basis_points) / 10_000
                    } else {
                        0
                    };
                    if penalty > 0 {
                        penalties.insert(player_id, penalty);
                    }
                    if vanity_tax > 0 {
                        vanity_taxes.insert(player_id, vanity_tax);
                        state.ledger.push(LedgerEntry {
                            guild_id: game.guild_id,
                            account_id: player_id,
                            delta: -vanity_tax,
                            source: "vanity_tax",
                            related_type: "mafia_game",
                            related_id: game_id.to_string(),
                        });
                    }
                    *state
                        .balances
                        .entry((game.guild_id, player_id))
                        .or_default() += gross - penalty - vanity_tax;
                    *state.nonprofit.entry(game.guild_id).or_default() += penalty + vanity_tax;
                }
                *state.nonprofit.entry(game.guild_id).or_default() += payout.nonprofit_overflow;
                if let Some(active) = state.games.get_mut(&game_id) {
                    active.phase = Phase::Resolved;
                    active.winner = Some(winner);
                }
                DayResolution {
                    resolved: true,
                    continued: false,
                    winner: Some(winner),
                    lynched_id,
                    mvp_id,
                    mvp_bonus: mvp_id
                        .and_then(|id| payout.deltas.get(&id).copied())
                        .map_or(0, |amount| amount - payout.payout_per_winner),
                    payout_per_winner: payout.payout_per_winner,
                    pot_total,
                    winning_ids,
                    bookie_id,
                    bookie_payout: bookie_id
                        .and_then(|id| payout.deltas.get(&id).copied())
                        .unwrap_or(0),
                    nonprofit_overflow: payout.nonprofit_overflow,
                    bankruptcy_penalties: penalties,
                    vanity_taxes,
                    bounty,
                    cap_forced: cap_reached && lynched_role != Some(Role::Jester),
                    ..DayResolution::default()
                }
            } else {
                if let Some(active) = state.games.get_mut(&game_id) {
                    active.phase = Phase::Night;
                    active.day_number += 1;
                }
                DayResolution {
                    resolved: true,
                    continued: true,
                    lynched_id,
                    bounty,
                    ..DayResolution::default()
                }
            };

            if state.post_commit_fault {
                state.post_commit_fault = false;
                return Err(ServiceError::PostCommitFault);
            }
            resolution.reason = None;
            Ok(resolution)
        })
    }

    pub fn force_finalize(&self, guild_id: GuildId) -> Result<DayResolution, ServiceError> {
        let game = self.repo.read(|state| {
            state
                .active_game_id(guild_id)
                .and_then(|id| state.games.get(&id).cloned())
        });
        let Some(game) = game else {
            return Ok(DayResolution {
                reason: Some("no_active_game"),
                ..DayResolution::default()
            });
        };
        if !self.repo.transaction(|state| {
            let Some(current) = state.games.get_mut(&game.game_id) else {
                return false;
            };
            if current.phase == Phase::Resolved {
                return false;
            }
            current.day_number = MAX_CYCLES;
            current.phase = Phase::Day;
            true
        }) {
            return Ok(DayResolution {
                reason: Some("already_resolved"),
                ..DayResolution::default()
            });
        }
        self.resolve_day(guild_id)
    }

    #[must_use]
    pub fn public_status(&self, guild_id: GuildId) -> PublicStatus {
        self.repo.read(|state| {
            let Some(game_id) = state.active_game_id(guild_id) else {
                return PublicStatus {
                    active: false,
                    phase: None,
                    alive_count: 0,
                    roster_size: 0,
                    voted_count: 0,
                    alive_voters: 0,
                };
            };
            let game = &state.games[&game_id];
            let players = &state.players[&game_id];
            let required: BTreeSet<_> = players
                .iter()
                .filter(|player| {
                    player.is_alive && !state.optouts.contains(&(guild_id, player.discord_id))
                })
                .map(|player| player.discord_id)
                .collect();
            let voted: BTreeSet<_> = state
                .actions
                .iter()
                .filter(|action| {
                    action.game_id == game_id
                        && action.day_number == game.day_number
                        && action.action_type == ActionType::Vote
                        && required.contains(&action.actor_id)
                })
                .map(|action| action.actor_id)
                .collect();
            PublicStatus {
                active: true,
                phase: Some(game.phase),
                alive_count: players.iter().filter(|player| player.is_alive).count(),
                roster_size: game.roster_size,
                voted_count: if game.phase == Phase::Day {
                    voted.len()
                } else {
                    0
                },
                alive_voters: if game.phase == Phase::Day {
                    required.len()
                } else {
                    0
                },
            }
        })
    }

    #[must_use]
    pub fn player_role(&self, guild_id: GuildId, player_id: PlayerId) -> Option<RoleView> {
        self.repo.read(|state| {
            let game_id = state.active_game_id(guild_id)?;
            let game = state.games.get(&game_id)?;
            let players = state.players.get(&game_id)?;
            let player = Self::find_player(players, player_id)?;
            let allies = if player.role == Role::Mafia {
                players
                    .iter()
                    .filter(|ally| ally.role == Role::Mafia && ally.discord_id != player.discord_id)
                    .map(|ally| Ally {
                        discord_id: ally.discord_id,
                        is_godfather: ally.is_godfather,
                        hero_name: ally.hero_name.clone(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let investigations = if player.role == Role::Detective {
                state
                    .actions
                    .iter()
                    .filter(|action| {
                        action.game_id == game_id
                            && action.actor_id == player_id
                            && action.action_type == ActionType::Investigate
                    })
                    .filter_map(|action| {
                        action
                            .result
                            .as_ref()
                            .map(|result| (action.target_id, result.clone()))
                    })
                    .collect()
            } else {
                Vec::new()
            };
            Some(RoleView {
                role: player.role,
                is_godfather: player.is_godfather,
                hero_name: player.hero_name.clone(),
                is_alive: player.is_alive,
                phase: game.phase,
                allies,
                investigations,
            })
        })
    }

    /// Repository-level finalize used by recovery/admin adapters. The phase
    /// claim, payout, vanity tax ledger, and terminal state are one transaction.
    pub fn finalize_manual(
        &self,
        game_id: GameId,
        winner: Winner,
        payout_deltas: &BTreeMap<PlayerId, i64>,
    ) -> bool {
        self.repo.transaction(|state| {
            let Some(game) = state.games.get(&game_id).cloned() else {
                return false;
            };
            if game.phase != Phase::Day {
                return false;
            }
            for (&player_id, &gross) in payout_deltas {
                let profit = (gross - game.entry_fee).max(0);
                let tax = if state.vanity_taxable.contains(&(game.guild_id, player_id)) {
                    profit * i64::from(self.vanity_tax_basis_points) / 10_000
                } else {
                    0
                };
                *state
                    .balances
                    .entry((game.guild_id, player_id))
                    .or_default() += gross - tax;
                if tax > 0 {
                    *state.nonprofit.entry(game.guild_id).or_default() += tax;
                    state.ledger.push(LedgerEntry {
                        guild_id: game.guild_id,
                        account_id: player_id,
                        delta: -tax,
                        source: "vanity_tax",
                        related_type: "mafia_game",
                        related_id: game_id.to_string(),
                    });
                }
            }
            if let Some(current) = state.games.get_mut(&game_id) {
                current.phase = Phase::Resolved;
                current.winner = Some(winner);
            }
            true
        })
    }
}

#[cfg(test)]
mod tests;
