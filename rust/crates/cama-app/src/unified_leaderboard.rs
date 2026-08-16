//! Typed application policy for the unified leaderboard view.
//!
//! This module owns the transport-independent behavior used by the live Rust
//! Discord runtime: one lazy state per tab, guild/member filtering before
//! display limits, deterministic ordering, pagination, and embed-shaped
//! rendering. Persistence is supplied through
//! [`UnifiedLeaderboardRepositoryPort`].

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use cama_domain::embed_safety::{EmbedModel, FIELD_VALUE_LIMIT, truncate_field};
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::openskill::CamaOpenSkillSystem;
use cama_domain::player::Player;
use cama_domain::rating::CamaRatingSystem;
use cama_domain::rating_insights::rd_to_certainty;
use cama_domain::tip_service::{TipLeaderboardEntry, TipVolume};

pub const SINGLE_SECTION_PAGE_SIZE: usize = 20;
pub const MULTI_SECTION_PAGE_SIZE: usize = 8;
pub const ALL_LEADERBOARD_ENTRIES_LIMIT: usize = i32::MAX as usize;
pub const DEFAULT_LEADERBOARD_LIMIT: usize = 100;
pub const TRIVIA_WINDOW_DAYS: i64 = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LeaderboardTab {
    Balance,
    Gambling,
    Glicko,
    OpenSkill,
    Tips,
    Trivia,
}

impl LeaderboardTab {
    pub const ALL: [Self; 6] = [
        Self::Balance,
        Self::Gambling,
        Self::Glicko,
        Self::OpenSkill,
        Self::Tips,
        Self::Trivia,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Balance => "balance",
            Self::Gambling => "gambling",
            Self::Glicko => "glicko",
            Self::OpenSkill => "openskill",
            Self::Tips => "tips",
            Self::Trivia => "trivia",
        }
    }

    #[must_use]
    pub fn from_type_parameter(value: Option<&str>) -> Self {
        match value
            .unwrap_or(Self::Balance.as_str())
            .to_ascii_lowercase()
            .as_str()
        {
            "gambling" => Self::Gambling,
            "glicko" => Self::Glicko,
            "openskill" => Self::OpenSkill,
            "tips" => Self::Tips,
            "trivia" => Self::Trivia,
            _ => Self::Balance,
        }
    }

    #[must_use]
    pub const fn page_size(self) -> usize {
        match self {
            Self::Gambling | Self::Tips => MULTI_SECTION_PAGE_SIZE,
            Self::Balance | Self::Glicko | Self::OpenSkill | Self::Trivia => {
                SINGLE_SECTION_PAGE_SIZE
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonStyle {
    Primary,
    Secondary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaginationButtons {
    pub previous_disabled: bool,
    pub next_disabled: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BankruptcyState {
    pub penalty_games_remaining: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabExtra {
    pub bankruptcy_states: BTreeMap<i64, BankruptcyState>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TabData {
    Balance(BalanceLeaderboard),
    Gambling(GamblingLeaderboard),
    Glicko(RatingLeaderboard),
    OpenSkill(RatingLeaderboard),
    Tips(TipsLeaderboard),
    Trivia(Vec<TriviaEntry>),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabState {
    pub data: Option<TabData>,
    pub current_page: usize,
    pub max_page: usize,
    pub loaded: bool,
    pub extra: TabExtra,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GuildMemberDirectory {
    names: BTreeMap<i64, String>,
}

impl GuildMemberDirectory {
    #[must_use]
    pub fn new(members: impl IntoIterator<Item = (i64, String)>) -> Self {
        Self {
            names: members.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn contains(&self, discord_id: i64) -> bool {
        self.names.contains_key(&discord_id)
    }

    #[must_use]
    pub fn display_name(&self, discord_id: i64) -> Option<&str> {
        self.names.get(&discord_id).map(String::as_str)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NegativeBalanceEntry {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub username: String,
    pub balance: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BalanceEntry {
    pub rank: usize,
    pub discord_id: i64,
    pub username: String,
    pub wins: i64,
    pub losses: i64,
    pub win_rate: f64,
    pub rating: Option<i64>,
    pub jopacoin_balance: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BalanceLeaderboard {
    pub players: Vec<BalanceEntry>,
    pub total_count: usize,
    pub debtors: Vec<NegativeBalanceEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RatingKind {
    Glicko,
    OpenSkill,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RatingEntry {
    pub rank: usize,
    pub discord_id: i64,
    pub username: String,
    pub rating_display: i64,
    pub certainty: f64,
    pub wins: i64,
    pub losses: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RatingLeaderboard {
    pub players: Vec<RatingEntry>,
    pub total_rated: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GamblingEntry {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub total_bets: i64,
    pub wins: i64,
    pub losses: i64,
    pub win_rate: f64,
    pub net_pnl: i64,
    pub total_wagered: i64,
    pub avg_leverage: f64,
    pub degen_score: Option<i64>,
    pub degen_title: Option<String>,
    pub degen_emoji: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GamblingServerStats {
    pub total_bets: i64,
    pub total_wagered: i64,
    pub unique_gamblers: i64,
    pub total_bankruptcies: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GamblingSnapshot {
    pub entries: Vec<GamblingEntry>,
    pub server_stats: Option<GamblingServerStats>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GamblingLeaderboard {
    pub top_earners: Vec<GamblingEntry>,
    pub down_bad: Vec<GamblingEntry>,
    pub hall_of_degen: Vec<GamblingEntry>,
    pub biggest_gamblers: Vec<GamblingEntry>,
    pub server_stats: Option<GamblingServerStats>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopedTipLeaderboardEntry {
    pub guild_id: Option<i64>,
    pub entry: TipLeaderboardEntry,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TipsSnapshot {
    pub top_senders: Vec<ScopedTipLeaderboardEntry>,
    pub top_receivers: Vec<ScopedTipLeaderboardEntry>,
    pub total_volume: TipVolume,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TipsLeaderboard {
    pub top_senders: Vec<TipLeaderboardEntry>,
    pub top_receivers: Vec<TipLeaderboardEntry>,
    pub total_volume: TipVolume,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriviaEntry {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub best_streak: i64,
}

/// Read contract for one existing-schema unified-leaderboard snapshot.
///
/// Every method receives the selected guild explicitly. Implementations must
/// perform guild-scoped reads; the application layer defensively rejects rows
/// carrying another normalized guild ID as well.
pub trait UnifiedLeaderboardRepositoryPort {
    type Error;

    fn balance_candidates(
        &mut self,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Player>, Self::Error>;

    fn player_count(&mut self, guild_id: Option<i64>) -> Result<usize, Self::Error>;

    fn negative_balances(
        &mut self,
        guild_id: Option<i64>,
    ) -> Result<Vec<NegativeBalanceEntry>, Self::Error>;

    fn gambling_snapshot(
        &mut self,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Option<GamblingSnapshot>, Self::Error>;

    fn bankruptcy_states(
        &mut self,
        guild_id: Option<i64>,
        discord_ids: &[i64],
    ) -> Result<BTreeMap<i64, BankruptcyState>, Self::Error>;

    fn rating_candidates(
        &mut self,
        guild_id: Option<i64>,
        kind: RatingKind,
        limit: usize,
    ) -> Result<Vec<Player>, Self::Error>;

    fn rated_player_count(
        &mut self,
        guild_id: Option<i64>,
        kind: RatingKind,
    ) -> Result<usize, Self::Error>;

    fn tips_snapshot(
        &mut self,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Option<TipsSnapshot>, Self::Error>;

    fn trivia_candidates(
        &mut self,
        guild_id: Option<i64>,
        days: i64,
        limit: usize,
    ) -> Result<Vec<TriviaEntry>, Self::Error>;
}

#[derive(Debug)]
pub struct UnifiedLeaderboardView<R> {
    repository: R,
    guild_id: Option<i64>,
    members: Option<GuildMemberDirectory>,
    current_tab: LeaderboardTab,
    limit: usize,
    tab_states: BTreeMap<LeaderboardTab, TabState>,
}

impl<R> UnifiedLeaderboardView<R>
where
    R: UnifiedLeaderboardRepositoryPort,
{
    #[must_use]
    pub fn new(
        repository: R,
        guild_id: Option<i64>,
        members: Option<GuildMemberDirectory>,
    ) -> Self {
        Self::with_options(
            repository,
            guild_id,
            members,
            LeaderboardTab::Balance,
            DEFAULT_LEADERBOARD_LIMIT,
        )
    }

    #[must_use]
    pub fn with_options(
        repository: R,
        guild_id: Option<i64>,
        members: Option<GuildMemberDirectory>,
        initial_tab: LeaderboardTab,
        limit: usize,
    ) -> Self {
        let tab_states = LeaderboardTab::ALL
            .into_iter()
            .map(|tab| (tab, TabState::default()))
            .collect();
        Self {
            repository,
            guild_id,
            members,
            current_tab: initial_tab,
            limit,
            tab_states,
        }
    }

    #[must_use]
    pub fn from_type_parameter(
        repository: R,
        guild_id: Option<i64>,
        members: Option<GuildMemberDirectory>,
        value: Option<&str>,
        limit: usize,
    ) -> Self {
        Self::with_options(
            repository,
            guild_id,
            members,
            LeaderboardTab::from_type_parameter(value),
            limit,
        )
    }

    #[must_use]
    pub const fn current_tab(&self) -> LeaderboardTab {
        self.current_tab
    }

    pub fn switch_tab(&mut self, tab: LeaderboardTab) {
        self.current_tab = tab;
    }

    #[must_use]
    pub fn tab_state(&self, tab: LeaderboardTab) -> &TabState {
        self.tab_states
            .get(&tab)
            .expect("every LeaderboardTab is initialized")
    }

    pub fn tab_state_mut(&mut self, tab: LeaderboardTab) -> &mut TabState {
        self.tab_states
            .get_mut(&tab)
            .expect("every LeaderboardTab is initialized")
    }

    #[must_use]
    pub const fn tab_count(&self) -> usize {
        LeaderboardTab::ALL.len()
    }

    #[must_use]
    pub fn button_style(&self, tab: LeaderboardTab) -> ButtonStyle {
        if tab == self.current_tab {
            ButtonStyle::Primary
        } else {
            ButtonStyle::Secondary
        }
    }

    #[must_use]
    pub fn pagination_buttons(&self) -> PaginationButtons {
        let state = self.tab_state(self.current_tab);
        PaginationButtons {
            previous_disabled: state.current_page == 0,
            next_disabled: state.current_page >= state.max_page,
        }
    }

    pub fn previous_page(&mut self) -> bool {
        let state = self.tab_state_mut(self.current_tab);
        if state.current_page == 0 {
            return false;
        }
        state.current_page -= 1;
        true
    }

    pub fn next_page(&mut self) -> bool {
        let state = self.tab_state_mut(self.current_tab);
        if state.current_page >= state.max_page {
            return false;
        }
        state.current_page += 1;
        true
    }

    #[must_use]
    pub const fn candidate_limit(&self) -> usize {
        if self.members.is_some() {
            ALL_LEADERBOARD_ENTRIES_LIMIT
        } else {
            self.limit
        }
    }

    #[must_use]
    pub const fn repository(&self) -> &R {
        &self.repository
    }

    pub const fn repository_mut(&mut self) -> &mut R {
        &mut self.repository
    }

    #[must_use]
    pub fn into_repository(self) -> R {
        self.repository
    }

    /// Load one tab exactly once. A repository error leaves the tab unloaded,
    /// allowing the caller to retry without observing a partial state.
    pub fn load_tab(&mut self, tab: LeaderboardTab) -> Result<bool, R::Error> {
        if self.tab_state(tab).loaded {
            return Ok(false);
        }

        let mut state = match tab {
            LeaderboardTab::Balance => self.fetch_balance()?,
            LeaderboardTab::Gambling => self.fetch_gambling()?,
            LeaderboardTab::Glicko => self.fetch_rating(RatingKind::Glicko)?,
            LeaderboardTab::OpenSkill => self.fetch_rating(RatingKind::OpenSkill)?,
            LeaderboardTab::Tips => self.fetch_tips()?,
            LeaderboardTab::Trivia => self.fetch_trivia()?,
        };
        state.loaded = true;
        self.tab_states.insert(tab, state);
        Ok(true)
    }

    fn fetch_balance(&mut self) -> Result<TabState, R::Error> {
        let candidate_limit = self.candidate_limit();
        let mut players = self
            .repository
            .balance_candidates(self.guild_id, candidate_limit)?;
        let repository_total = self.repository.player_count(self.guild_id)?;
        let mut debtors = self.repository.negative_balances(self.guild_id)?;

        players.retain(|player| {
            player.discord_id.is_some()
                && same_guild(player.guild_id, self.guild_id)
                && self.is_visible_member(player.discord_id.unwrap_or_default())
        });
        players.sort_by(compare_balance_players);
        let filtered_total = players.len();
        players.truncate(self.limit);

        let rating_system = CamaRatingSystem::default();
        let players = players
            .into_iter()
            .enumerate()
            .map(|(index, player)| {
                let discord_id = player.discord_id.expect("retained players have IDs");
                let games = player.wins.saturating_add(player.losses);
                BalanceEntry {
                    rank: index + 1,
                    discord_id,
                    username: player.name,
                    wins: player.wins,
                    losses: player.losses,
                    win_rate: if games > 0 {
                        player.wins as f64 / games as f64 * 100.0
                    } else {
                        0.0
                    },
                    rating: player
                        .glicko_rating
                        .map(|rating| rating_system.rating_to_display(rating)),
                    jopacoin_balance: player.jopacoin_balance,
                }
            })
            .collect::<Vec<_>>();

        debtors.retain(|entry| {
            same_guild(entry.guild_id, self.guild_id) && self.is_visible_member(entry.discord_id)
        });
        debtors.sort_by(|left, right| {
            left.balance
                .cmp(&right.balance)
                .then_with(|| left.discord_id.cmp(&right.discord_id))
        });

        let total_count = if self.members.is_some() {
            filtered_total
        } else {
            repository_total
        };
        Ok(loaded_state(
            TabData::Balance(BalanceLeaderboard {
                players,
                total_count,
                debtors,
            }),
            max_page(filtered_total.min(self.limit), SINGLE_SECTION_PAGE_SIZE),
        ))
    }

    fn fetch_rating(&mut self, kind: RatingKind) -> Result<TabState, R::Error> {
        let candidate_limit = self.candidate_limit();
        let mut players =
            self.repository
                .rating_candidates(self.guild_id, kind, candidate_limit)?;
        let repository_total = self.repository.rated_player_count(self.guild_id, kind)?;
        players.retain(|player| {
            player.discord_id.is_some()
                && same_guild(player.guild_id, self.guild_id)
                && self.is_visible_member(player.discord_id.unwrap_or_default())
                && rating_value(player, kind).is_some()
        });
        players.sort_by(|left, right| compare_rating_players(left, right, kind));
        let filtered_total = players.len();
        players.truncate(self.limit);

        let glicko = CamaRatingSystem::default();
        let openskill = CamaOpenSkillSystem::default();
        let entries = players
            .into_iter()
            .enumerate()
            .map(|(index, player)| {
                let discord_id = player.discord_id.expect("retained players have IDs");
                let (rating_display, certainty) = match kind {
                    RatingKind::Glicko => (
                        glicko.rating_to_display(
                            player
                                .glicko_rating
                                .expect("retained Glicko players have ratings"),
                        ),
                        rd_to_certainty(player.glicko_rd.unwrap_or(350.0)),
                    ),
                    RatingKind::OpenSkill => (
                        i64::from(
                            openskill.mu_to_display(
                                player
                                    .os_mu
                                    .expect("retained OpenSkill players have ratings"),
                            ),
                        ),
                        openskill.certainty_percentage(
                            player
                                .os_sigma
                                .unwrap_or(CamaOpenSkillSystem::DEFAULT_SIGMA),
                        ),
                    ),
                };
                RatingEntry {
                    rank: index + 1,
                    discord_id,
                    username: player.name,
                    rating_display,
                    certainty,
                    wins: player.wins,
                    losses: player.losses,
                }
            })
            .collect::<Vec<_>>();

        let total_rated = if self.members.is_some() {
            filtered_total
        } else {
            repository_total
        };
        let data = RatingLeaderboard {
            players: entries,
            total_rated,
        };
        let tab_data = match kind {
            RatingKind::Glicko => TabData::Glicko(data),
            RatingKind::OpenSkill => TabData::OpenSkill(data),
        };
        Ok(loaded_state(
            tab_data,
            max_page(filtered_total.min(self.limit), SINGLE_SECTION_PAGE_SIZE),
        ))
    }

    fn fetch_gambling(&mut self) -> Result<TabState, R::Error> {
        let candidate_limit = self.candidate_limit();
        let Some(mut snapshot) = self
            .repository
            .gambling_snapshot(self.guild_id, candidate_limit)?
        else {
            return Ok(TabState::default());
        };
        snapshot.entries.retain(|entry| {
            same_guild(entry.guild_id, self.guild_id) && self.is_visible_member(entry.discord_id)
        });

        let mut top_earners = snapshot
            .entries
            .iter()
            .filter(|entry| entry.net_pnl >= 0)
            .cloned()
            .collect::<Vec<_>>();
        top_earners.sort_by(|left, right| {
            right
                .net_pnl
                .cmp(&left.net_pnl)
                .then_with(|| left.discord_id.cmp(&right.discord_id))
        });

        let mut down_bad = snapshot
            .entries
            .iter()
            .filter(|entry| entry.net_pnl < 0)
            .cloned()
            .collect::<Vec<_>>();
        down_bad.sort_by(|left, right| {
            left.net_pnl
                .cmp(&right.net_pnl)
                .then_with(|| left.discord_id.cmp(&right.discord_id))
        });

        let mut hall_of_degen = snapshot.entries.clone();
        hall_of_degen.sort_by(|left, right| {
            right
                .degen_score
                .unwrap_or_default()
                .cmp(&left.degen_score.unwrap_or_default())
                .then_with(|| left.discord_id.cmp(&right.discord_id))
        });

        let mut biggest_gamblers = snapshot.entries;
        biggest_gamblers.sort_by(|left, right| {
            right
                .total_wagered
                .cmp(&left.total_wagered)
                .then_with(|| left.discord_id.cmp(&right.discord_id))
        });

        for section in [
            &mut top_earners,
            &mut down_bad,
            &mut hall_of_degen,
            &mut biggest_gamblers,
        ] {
            section.truncate(self.limit);
        }

        let ids = top_earners
            .iter()
            .chain(&down_bad)
            .chain(&hall_of_degen)
            .chain(&biggest_gamblers)
            .map(|entry| entry.discord_id)
            .collect::<BTreeSet<_>>();
        let mut bankruptcy_states = if ids.is_empty() {
            BTreeMap::new()
        } else {
            self.repository
                .bankruptcy_states(self.guild_id, &ids.iter().copied().collect::<Vec<_>>())?
        };
        bankruptcy_states.retain(|discord_id, _| ids.contains(discord_id));

        let max_entries = [
            top_earners.len(),
            down_bad.len(),
            hall_of_degen.len(),
            biggest_gamblers.len(),
        ]
        .into_iter()
        .max()
        .unwrap_or_default();
        let mut state = loaded_state(
            TabData::Gambling(GamblingLeaderboard {
                top_earners,
                down_bad,
                hall_of_degen,
                biggest_gamblers,
                server_stats: snapshot.server_stats,
            }),
            max_page(max_entries, MULTI_SECTION_PAGE_SIZE),
        );
        state.extra.bankruptcy_states = bankruptcy_states;
        Ok(state)
    }

    fn fetch_tips(&mut self) -> Result<TabState, R::Error> {
        let candidate_limit = self.candidate_limit();
        let Some(mut snapshot) = self
            .repository
            .tips_snapshot(self.guild_id, candidate_limit)?
        else {
            return Ok(TabState::default());
        };
        for section in [&mut snapshot.top_senders, &mut snapshot.top_receivers] {
            section.retain(|scoped| {
                same_guild(scoped.guild_id, self.guild_id)
                    && self.is_visible_member(scoped.entry.discord_id)
            });
            section.sort_by(|left, right| {
                right
                    .entry
                    .total_amount
                    .cmp(&left.entry.total_amount)
                    .then_with(|| right.entry.tip_count.cmp(&left.entry.tip_count))
                    .then_with(|| left.entry.discord_id.cmp(&right.entry.discord_id))
            });
            section.truncate(self.limit);
        }
        let top_senders = snapshot
            .top_senders
            .into_iter()
            .map(|entry| entry.entry)
            .collect::<Vec<_>>();
        let top_receivers = snapshot
            .top_receivers
            .into_iter()
            .map(|entry| entry.entry)
            .collect::<Vec<_>>();
        let max_entries = top_senders.len().max(top_receivers.len());
        Ok(loaded_state(
            TabData::Tips(TipsLeaderboard {
                top_senders,
                top_receivers,
                total_volume: snapshot.total_volume,
            }),
            max_page(max_entries, MULTI_SECTION_PAGE_SIZE),
        ))
    }

    fn fetch_trivia(&mut self) -> Result<TabState, R::Error> {
        let candidate_limit = self.candidate_limit();
        let mut entries = self.repository.trivia_candidates(
            self.guild_id,
            TRIVIA_WINDOW_DAYS,
            candidate_limit,
        )?;
        entries.retain(|entry| {
            same_guild(entry.guild_id, self.guild_id) && self.is_visible_member(entry.discord_id)
        });
        entries.sort_by(|left, right| {
            right
                .best_streak
                .cmp(&left.best_streak)
                .then_with(|| left.discord_id.cmp(&right.discord_id))
        });
        entries.truncate(self.limit);
        let page = max_page(entries.len(), SINGLE_SECTION_PAGE_SIZE);
        Ok(loaded_state(TabData::Trivia(entries), page))
    }

    fn is_visible_member(&self, discord_id: i64) -> bool {
        self.members
            .as_ref()
            .is_none_or(|members| members.contains(discord_id))
    }

    fn display_name(&self, discord_id: i64) -> String {
        self.members
            .as_ref()
            .and_then(|members| members.display_name(discord_id))
            .map_or_else(|| format!("User {discord_id}"), str::to_owned)
    }

    #[must_use]
    pub fn build_embed(&self) -> EmbedModel {
        let state = self.tab_state(self.current_tab);
        match self.current_tab {
            LeaderboardTab::Balance => self.build_balance_embed(state),
            LeaderboardTab::Gambling => self.build_gambling_embed(state),
            LeaderboardTab::Glicko => self.build_rating_embed(state, RatingKind::Glicko),
            LeaderboardTab::OpenSkill => self.build_rating_embed(state, RatingKind::OpenSkill),
            LeaderboardTab::Tips => self.build_tips_embed(state),
            LeaderboardTab::Trivia => self.build_trivia_embed(state),
        }
    }

    fn build_balance_embed(&self, state: &TabState) -> EmbedModel {
        let mut embed = titled_embed("LEADERBOARD > Balance");
        let Some(TabData::Balance(data)) = state.data.as_ref() else {
            embed.description = Some("No players registered yet!".to_owned());
            return embed;
        };
        if data.players.is_empty() {
            embed.description = Some("No players registered yet!".to_owned());
            return embed;
        }

        let (start, end) = page_bounds(state, SINGLE_SECTION_PAGE_SIZE, data.players.len());
        let lines = data.players[start..end]
            .iter()
            .map(|entry| {
                let marker = rank_marker(entry.rank);
                let name = if entry.discord_id > 0 {
                    format!("<@{}>", entry.discord_id)
                } else {
                    entry.username.clone()
                };
                let games = entry.wins.saturating_add(entry.losses);
                let record = if games > 0 {
                    format!("{}-{} ({:.0}%)", entry.wins, entry.losses, entry.win_rate)
                } else {
                    format!("{}-{}", entry.wins, entry.losses)
                };
                let rating = entry
                    .rating
                    .map_or_else(String::new, |rating| format!(" [{rating}]"));
                format!(
                    "{marker} **{name}** - {} {JOPACOIN_EMOTE} - {record}{rating}",
                    entry.jopacoin_balance
                )
            })
            .collect::<Vec<_>>();
        embed.description = Some(lines.join("\n"));

        if state.current_page == 0 && !data.debtors.is_empty() {
            let shame = data
                .debtors
                .iter()
                .take(10)
                .enumerate()
                .map(|(index, debtor)| {
                    let name = if debtor.discord_id > 0 {
                        format!("<@{}>", debtor.discord_id)
                    } else {
                        debtor.username.clone()
                    };
                    format!(
                        "{}. {name} - {} {JOPACOIN_EMOTE}",
                        index + 1,
                        debtor.balance
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            embed.add_field("Wall of Shame", shame, false);
        }
        embed.footer = Some(page_footer(
            state,
            Some((data.players.len(), data.total_count, "players")),
        ));
        embed
    }

    fn build_gambling_embed(&self, state: &TabState) -> EmbedModel {
        let mut embed = titled_embed("LEADERBOARD > Gambling");
        let Some(TabData::Gambling(data)) = state.data.as_ref() else {
            embed.description = Some("Gambling stats service is not available.".to_owned());
            return embed;
        };
        if data.top_earners.is_empty() && data.hall_of_degen.is_empty() {
            embed.description = Some(
                "No gambling data yet! Players need at least 3 settled bets to appear.".to_owned(),
            );
            return embed;
        }
        let start = state.current_page.saturating_mul(MULTI_SECTION_PAGE_SIZE);
        self.add_gambling_section(
            &mut embed,
            "Top Earners",
            &data.top_earners,
            start,
            |entry, name| {
                let sign = if entry.net_pnl >= 0 { "+" } else { "" };
                format!(
                    "**{name}** {sign}{} {JOPACOIN_EMOTE} ({:.0}%)",
                    entry.net_pnl,
                    entry.win_rate * 100.0
                )
            },
        );
        self.add_gambling_section(
            &mut embed,
            "Down Bad",
            &data.down_bad,
            start,
            |entry, name| {
                format!(
                    "**{name}** {} {JOPACOIN_EMOTE} ({:.0}%)",
                    entry.net_pnl,
                    entry.win_rate * 100.0
                )
            },
        );
        self.add_gambling_section(
            &mut embed,
            "Hall of Degen",
            &data.hall_of_degen,
            start,
            |entry, name| {
                format!(
                    "**{name}** {} {} {}",
                    entry.degen_score.unwrap_or_default(),
                    entry.degen_emoji.as_deref().unwrap_or_default(),
                    entry.degen_title.as_deref().unwrap_or_default()
                )
            },
        );
        self.add_gambling_section(
            &mut embed,
            "Biggest Gamblers",
            &data.biggest_gamblers,
            start,
            |entry, name| {
                format!(
                    "**{name}** {} {JOPACOIN_EMOTE} wagered",
                    entry.total_wagered
                )
            },
        );

        let page = format!("Page {}/{}", state.current_page + 1, state.max_page + 1);
        embed.footer = Some(data.server_stats.map_or(page.clone(), |stats| {
            format!(
                "{} bets • {} JC wagered • {} players • {} bankruptcies | {page}",
                stats.total_bets,
                stats.total_wagered,
                stats.unique_gamblers,
                stats.total_bankruptcies
            )
        }));
        embed
    }

    fn add_gambling_section<F>(
        &self,
        embed: &mut EmbedModel,
        title: &str,
        entries: &[GamblingEntry],
        start: usize,
        format_entry: F,
    ) where
        F: Fn(&GamblingEntry, String) -> String,
    {
        let end = start
            .saturating_add(MULTI_SECTION_PAGE_SIZE)
            .min(entries.len());
        if start >= end {
            return;
        }
        let lines = entries[start..end]
            .iter()
            .enumerate()
            .map(|(offset, entry)| {
                let mut name = self.display_name(entry.discord_id);
                if self
                    .tab_state(LeaderboardTab::Gambling)
                    .extra
                    .bankruptcy_states
                    .get(&entry.discord_id)
                    .is_some_and(|state| state.penalty_games_remaining > 0)
                {
                    name = format!("🪦 {name}");
                }
                format!("{}. {}", start + offset + 1, format_entry(entry, name))
            })
            .collect::<Vec<_>>()
            .join("\n");
        embed.add_field(title, truncate_field(&lines, FIELD_VALUE_LIMIT), false);
    }

    fn build_rating_embed(&self, state: &TabState, kind: RatingKind) -> EmbedModel {
        let (title, subtitle, empty) = match kind {
            RatingKind::Glicko => (
                "LEADERBOARD > Glicko-2 Rating",
                "Primary inhouse skill rating",
                "No players with Glicko-2 ratings yet!",
            ),
            RatingKind::OpenSkill => (
                "LEADERBOARD > OpenSkill Rating",
                "Fantasy-weighted performance rating",
                "No players with OpenSkill ratings yet!",
            ),
        };
        let mut embed = titled_embed(title);
        embed.description = Some(subtitle.to_owned());
        let data = match (&state.data, kind) {
            (Some(TabData::Glicko(data)), RatingKind::Glicko)
            | (Some(TabData::OpenSkill(data)), RatingKind::OpenSkill) => data,
            _ => {
                embed.description = Some(empty.to_owned());
                return embed;
            }
        };
        if data.players.is_empty() {
            embed.description = Some(empty.to_owned());
            return embed;
        }
        let (start, end) = page_bounds(state, SINGLE_SECTION_PAGE_SIZE, data.players.len());
        let lines = data.players[start..end]
            .iter()
            .map(|entry| {
                let name = if entry.discord_id > 0 {
                    format!("<@{}>", entry.discord_id)
                } else {
                    entry.username.clone()
                };
                format_rating_entry(entry, &name)
            })
            .collect::<Vec<_>>()
            .join("\n");
        embed.add_field("Rankings", truncate_field(&lines, FIELD_VALUE_LIMIT), false);
        embed.footer = Some(page_footer(
            state,
            Some((data.players.len(), data.total_rated, "rated players")),
        ));
        embed
    }

    fn build_tips_embed(&self, state: &TabState) -> EmbedModel {
        let mut embed = titled_embed("LEADERBOARD > Tips");
        embed.description = Some("Jopacoin tipping rankings".to_owned());
        let Some(TabData::Tips(data)) = state.data.as_ref() else {
            embed.description = Some("Tip tracking service is not available.".to_owned());
            return embed;
        };
        if data.top_senders.is_empty() && data.top_receivers.is_empty() {
            embed.description =
                Some("No tips yet! Use `/economy tip` to spread some jopacoin love.".to_owned());
            return embed;
        }
        let start = state.current_page.saturating_mul(MULTI_SECTION_PAGE_SIZE);
        self.add_tip_section(&mut embed, "💝 Most Generous", &data.top_senders, start);
        self.add_tip_section(&mut embed, "⭐ Fan Favorites", &data.top_receivers, start);
        embed.add_field(
            "📊 Server Stats",
            format!(
                "**Total Tips:** {}\n**Total Tipped:** {} {JOPACOIN_EMOTE}\n**Fees Collected:** {} {JOPACOIN_EMOTE}",
                data.total_volume.total_transactions,
                data.total_volume.total_amount,
                data.total_volume.total_fees
            ),
            false,
        );
        embed.footer = Some(format!(
            "Page {}/{}",
            state.current_page + 1,
            state.max_page + 1
        ));
        embed
    }

    fn add_tip_section(
        &self,
        embed: &mut EmbedModel,
        title: &str,
        entries: &[TipLeaderboardEntry],
        start: usize,
    ) {
        let end = start
            .saturating_add(MULTI_SECTION_PAGE_SIZE)
            .min(entries.len());
        if start >= end {
            return;
        }
        let lines = entries[start..end]
            .iter()
            .enumerate()
            .map(|(offset, entry)| {
                format!(
                    "{}. **{}** {} {JOPACOIN_EMOTE} ({} tips)",
                    start + offset + 1,
                    self.display_name(entry.discord_id),
                    entry.total_amount,
                    entry.tip_count
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        embed.add_field(title, truncate_field(&lines, FIELD_VALUE_LIMIT), false);
    }

    fn build_trivia_embed(&self, state: &TabState) -> EmbedModel {
        let mut embed = titled_embed("LEADERBOARD > Trivia");
        embed.description = Some("Best streaks in the last 7 days".to_owned());
        let Some(TabData::Trivia(entries)) = state.data.as_ref() else {
            embed.description = Some("No trivia sessions in the last 7 days.".to_owned());
            return embed;
        };
        if entries.is_empty() {
            embed.description = Some("No trivia sessions in the last 7 days.".to_owned());
            return embed;
        }
        let (start, end) = page_bounds(state, SINGLE_SECTION_PAGE_SIZE, entries.len());
        let lines = entries[start..end]
            .iter()
            .enumerate()
            .map(|(offset, entry)| {
                let rank = start + offset + 1;
                format!(
                    "{} **{}** — streak of **{}**",
                    rank_marker(rank),
                    self.display_name(entry.discord_id),
                    entry.best_streak
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        embed.add_field("Best Streaks", lines, false);
        embed.footer = Some(format!(
            "Page {}/{}",
            state.current_page + 1,
            state.max_page + 1
        ));
        embed
    }
}

fn loaded_state(data: TabData, max_page: usize) -> TabState {
    TabState {
        data: Some(data),
        max_page,
        ..TabState::default()
    }
}

const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
    match guild_id {
        Some(guild_id) => guild_id,
        None => 0,
    }
}

fn same_guild(row_guild_id: Option<i64>, requested_guild_id: Option<i64>) -> bool {
    normalize_guild_id(row_guild_id) == normalize_guild_id(requested_guild_id)
}

fn compare_balance_players(left: &Player, right: &Player) -> Ordering {
    right
        .jopacoin_balance
        .cmp(&left.jopacoin_balance)
        .then_with(|| right.wins.cmp(&left.wins))
        .then_with(|| {
            right
                .glicko_rating
                .unwrap_or_default()
                .total_cmp(&left.glicko_rating.unwrap_or_default())
        })
        .then_with(|| {
            left.discord_id
                .unwrap_or_default()
                .cmp(&right.discord_id.unwrap_or_default())
        })
}

fn rating_value(player: &Player, kind: RatingKind) -> Option<f64> {
    match kind {
        RatingKind::Glicko => player.glicko_rating,
        RatingKind::OpenSkill => player.os_mu,
    }
}

fn compare_rating_players(left: &Player, right: &Player, kind: RatingKind) -> Ordering {
    let left_rating = rating_value(left, kind).expect("retained rating candidate");
    let right_rating = rating_value(right, kind).expect("retained rating candidate");
    right_rating
        .total_cmp(&left_rating)
        .then_with(|| right.wins.cmp(&left.wins))
        .then_with(|| {
            left.discord_id
                .unwrap_or_default()
                .cmp(&right.discord_id.unwrap_or_default())
        })
}

const fn max_page(entry_count: usize, page_size: usize) -> usize {
    entry_count.saturating_sub(1) / page_size
}

fn titled_embed(title: &str) -> EmbedModel {
    EmbedModel {
        title: Some(title.to_owned()),
        ..EmbedModel::default()
    }
}

fn page_bounds(state: &TabState, page_size: usize, len: usize) -> (usize, usize) {
    let start = state.current_page.saturating_mul(page_size).min(len);
    (start, start.saturating_add(page_size).min(len))
}

fn rank_marker(rank: usize) -> String {
    match rank {
        1 => "🥇".to_owned(),
        2 => "🥈".to_owned(),
        3 => "🥉".to_owned(),
        _ => format!("{rank}."),
    }
}

fn format_rating_entry(entry: &RatingEntry, display_name: &str) -> String {
    format!(
        "{} **{display_name}** {} ({:.0}%) {}-{}",
        rank_marker(entry.rank),
        entry.rating_display,
        entry.certainty,
        entry.wins,
        entry.losses
    )
}

fn page_footer(state: &TabState, count: Option<(usize, usize, &'static str)>) -> String {
    let mut footer = format!("Page {}/{}", state.current_page + 1, state.max_page + 1);
    if let Some((shown, total, noun)) = count
        && total > shown
    {
        footer.push_str(&format!(" • Showing {shown} of {total} {noun}"));
    }
    footer
}

#[cfg(test)]
mod tests;
