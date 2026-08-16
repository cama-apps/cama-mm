use std::collections::BTreeMap;
use std::convert::Infallible;

use cama_domain::player::Player;
use cama_domain::tip_service::{TipLeaderboardEntry, TipVolume};

use super::*;

const GUILD: i64 = 12_345;

#[derive(Clone, Debug, PartialEq, Eq)]
enum RepositoryCall {
    BalanceCandidates {
        guild_id: Option<i64>,
        limit: usize,
    },
    PlayerCount {
        guild_id: Option<i64>,
    },
    NegativeBalances {
        guild_id: Option<i64>,
    },
    GamblingSnapshot {
        guild_id: Option<i64>,
        limit: usize,
    },
    BankruptcyStates {
        guild_id: Option<i64>,
        discord_ids: Vec<i64>,
    },
    RatingCandidates {
        guild_id: Option<i64>,
        kind: RatingKind,
        limit: usize,
    },
    RatedPlayerCount {
        guild_id: Option<i64>,
        kind: RatingKind,
    },
    TipsSnapshot {
        guild_id: Option<i64>,
        limit: usize,
    },
    TriviaCandidates {
        guild_id: Option<i64>,
        days: i64,
        limit: usize,
    },
}

#[derive(Clone, Debug)]
struct FakeRepository {
    balance: Vec<Player>,
    player_count: usize,
    debtors: Vec<NegativeBalanceEntry>,
    gambling: Option<GamblingSnapshot>,
    bankruptcies: BTreeMap<i64, BankruptcyState>,
    glicko: Vec<Player>,
    openskill: Vec<Player>,
    glicko_count: usize,
    openskill_count: usize,
    tips: Option<TipsSnapshot>,
    trivia: Vec<TriviaEntry>,
    calls: Vec<RepositoryCall>,
}

impl Default for FakeRepository {
    fn default() -> Self {
        Self {
            balance: Vec::new(),
            player_count: 0,
            debtors: Vec::new(),
            gambling: Some(GamblingSnapshot::default()),
            bankruptcies: BTreeMap::new(),
            glicko: Vec::new(),
            openskill: Vec::new(),
            glicko_count: 0,
            openskill_count: 0,
            tips: None,
            trivia: Vec::new(),
            calls: Vec::new(),
        }
    }
}

impl UnifiedLeaderboardRepositoryPort for FakeRepository {
    type Error = Infallible;

    fn balance_candidates(
        &mut self,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Player>, Self::Error> {
        self.calls
            .push(RepositoryCall::BalanceCandidates { guild_id, limit });
        Ok(self.balance.iter().take(limit).cloned().collect())
    }

    fn player_count(&mut self, guild_id: Option<i64>) -> Result<usize, Self::Error> {
        self.calls.push(RepositoryCall::PlayerCount { guild_id });
        Ok(self.player_count)
    }

    fn negative_balances(
        &mut self,
        guild_id: Option<i64>,
    ) -> Result<Vec<NegativeBalanceEntry>, Self::Error> {
        self.calls
            .push(RepositoryCall::NegativeBalances { guild_id });
        Ok(self.debtors.clone())
    }

    fn gambling_snapshot(
        &mut self,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Option<GamblingSnapshot>, Self::Error> {
        self.calls
            .push(RepositoryCall::GamblingSnapshot { guild_id, limit });
        Ok(self.gambling.clone().map(|mut snapshot| {
            snapshot.entries.truncate(limit);
            snapshot
        }))
    }

    fn bankruptcy_states(
        &mut self,
        guild_id: Option<i64>,
        discord_ids: &[i64],
    ) -> Result<BTreeMap<i64, BankruptcyState>, Self::Error> {
        self.calls.push(RepositoryCall::BankruptcyStates {
            guild_id,
            discord_ids: discord_ids.to_vec(),
        });
        Ok(self.bankruptcies.clone())
    }

    fn rating_candidates(
        &mut self,
        guild_id: Option<i64>,
        kind: RatingKind,
        limit: usize,
    ) -> Result<Vec<Player>, Self::Error> {
        self.calls.push(RepositoryCall::RatingCandidates {
            guild_id,
            kind,
            limit,
        });
        let players = match kind {
            RatingKind::Glicko => &self.glicko,
            RatingKind::OpenSkill => &self.openskill,
        };
        Ok(players.iter().take(limit).cloned().collect())
    }

    fn rated_player_count(
        &mut self,
        guild_id: Option<i64>,
        kind: RatingKind,
    ) -> Result<usize, Self::Error> {
        self.calls
            .push(RepositoryCall::RatedPlayerCount { guild_id, kind });
        Ok(match kind {
            RatingKind::Glicko => self.glicko_count,
            RatingKind::OpenSkill => self.openskill_count,
        })
    }

    fn tips_snapshot(
        &mut self,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Option<TipsSnapshot>, Self::Error> {
        self.calls
            .push(RepositoryCall::TipsSnapshot { guild_id, limit });
        Ok(self.tips.clone().map(|mut snapshot| {
            snapshot.top_senders.truncate(limit);
            snapshot.top_receivers.truncate(limit);
            snapshot
        }))
    }

    fn trivia_candidates(
        &mut self,
        guild_id: Option<i64>,
        days: i64,
        limit: usize,
    ) -> Result<Vec<TriviaEntry>, Self::Error> {
        self.calls.push(RepositoryCall::TriviaCandidates {
            guild_id,
            days,
            limit,
        });
        Ok(self.trivia.iter().take(limit).cloned().collect())
    }
}

fn member_directory() -> GuildMemberDirectory {
    GuildMemberDirectory::new([
        (123, "User123".to_owned()),
        (456, "User456".to_owned()),
        (789, "User789".to_owned()),
        (67_890, "TestUser".to_owned()),
    ])
}

fn player(discord_id: i64, guild_id: i64, score: i64) -> Player {
    Player {
        discord_id: Some(discord_id),
        guild_id: Some(guild_id),
        wins: 1,
        losses: 1,
        glicko_rating: Some(score as f64),
        glicko_rd: Some(80.0),
        os_mu: Some(score as f64 / 100.0),
        os_sigma: Some(2.0),
        jopacoin_balance: score,
        ..Player::new(format!("Player{discord_id}"))
    }
}

fn gambling_entry(discord_id: i64, guild_id: i64, score: i64) -> GamblingEntry {
    GamblingEntry {
        discord_id,
        guild_id: Some(guild_id),
        total_bets: 5,
        wins: 3,
        losses: 2,
        win_rate: 0.6,
        net_pnl: score,
        total_wagered: score.unsigned_abs() as i64 + 100,
        avg_leverage: 1.0,
        degen_score: Some(score.unsigned_abs() as i64),
        degen_title: Some("Degen".to_owned()),
        degen_emoji: Some("🎰".to_owned()),
    }
}

fn paged_gambling_entries(count: usize) -> Vec<GamblingEntry> {
    (0..count)
        .map(|index| {
            let index = i64::try_from(index).expect("small fixture index");
            gambling_entry(1_000 + index, GUILD, 10_000 - index * 10)
        })
        .collect()
}

fn gambling_view(
    entries: Vec<GamblingEntry>,
    members: Option<GuildMemberDirectory>,
    server_stats: Option<GamblingServerStats>,
) -> UnifiedLeaderboardView<FakeRepository> {
    let repository = FakeRepository {
        gambling: Some(GamblingSnapshot {
            entries,
            server_stats,
        }),
        ..FakeRepository::default()
    };
    let mut view = UnifiedLeaderboardView::with_options(
        repository,
        Some(GUILD),
        members,
        LeaderboardTab::Gambling,
        100,
    );
    view.load_tab(LeaderboardTab::Gambling)
        .expect("load gambling tab");
    view
}

fn tip_entry(discord_id: i64, guild_id: i64, total_amount: i64) -> ScopedTipLeaderboardEntry {
    ScopedTipLeaderboardEntry {
        guild_id: Some(guild_id),
        entry: TipLeaderboardEntry {
            discord_id,
            total_amount,
            tip_count: 2,
        },
    }
}

fn view(
    repository: FakeRepository,
    initial_tab: LeaderboardTab,
    limit: usize,
) -> UnifiedLeaderboardView<FakeRepository> {
    UnifiedLeaderboardView::with_options(
        repository,
        Some(GUILD),
        Some(member_directory()),
        initial_tab,
        limit,
    )
}

fn title(embed: &cama_domain::embed_safety::EmbedModel) -> &str {
    embed.title.as_deref().expect("embed title")
}

fn description(embed: &cama_domain::embed_safety::EmbedModel) -> &str {
    embed.description.as_deref().expect("embed description")
}

#[test]
fn test_tab_values() {
    assert_eq!(
        LeaderboardTab::ALL.map(LeaderboardTab::as_str),
        [
            "balance",
            "gambling",
            "glicko",
            "openskill",
            "tips",
            "trivia"
        ]
    );
}

#[test]
fn test_tab_count() {
    assert_eq!(LeaderboardTab::ALL.len(), 6);
}

#[test]
fn test_tab_state_default_values() {
    let state = TabState::default();
    assert!(state.data.is_none());
    assert_eq!(state.current_page, 0);
    assert_eq!(state.max_page, 0);
    assert!(!state.loaded);
    assert_eq!(state.extra, TabExtra::default());
}

#[test]
fn test_tab_state_custom_values() {
    let state = TabState {
        data: Some(TabData::Trivia(vec![TriviaEntry {
            discord_id: 123,
            guild_id: Some(GUILD),
            best_streak: 9,
        }])),
        current_page: 2,
        max_page: 5,
        loaded: true,
        extra: TabExtra {
            bankruptcy_states: BTreeMap::from([(
                123,
                BankruptcyState {
                    penalty_games_remaining: 2,
                },
            )]),
        },
    };
    assert!(matches!(state.data, Some(TabData::Trivia(_))));
    assert_eq!((state.current_page, state.max_page), (2, 5));
    assert!(state.loaded);
    assert_eq!(state.extra.bankruptcy_states.len(), 1);
}

#[test]
fn test_initial_tab_balance() {
    let view = UnifiedLeaderboardView::new(
        FakeRepository::default(),
        Some(GUILD),
        Some(member_directory()),
    );
    assert_eq!(view.current_tab(), LeaderboardTab::Balance);
}

#[test]
fn test_initial_tab_custom() {
    let view = view(FakeRepository::default(), LeaderboardTab::Gambling, 100);
    assert_eq!(view.current_tab(), LeaderboardTab::Gambling);
}

#[test]
fn test_all_tabs_have_state() {
    let view = view(FakeRepository::default(), LeaderboardTab::Balance, 100);
    assert_eq!(view.tab_count(), 6);
    assert!(
        LeaderboardTab::ALL
            .into_iter()
            .all(|tab| view.tab_state(tab) == &TabState::default())
    );
}

#[test]
fn test_active_tab_is_primary() {
    let view = view(FakeRepository::default(), LeaderboardTab::Balance, 100);
    assert_eq!(
        view.button_style(LeaderboardTab::Balance),
        ButtonStyle::Primary
    );
    for tab in [
        LeaderboardTab::Gambling,
        LeaderboardTab::Glicko,
        LeaderboardTab::OpenSkill,
    ] {
        assert_eq!(view.button_style(tab), ButtonStyle::Secondary);
    }
}

#[test]
fn test_tab_switch_updates_styles() {
    let mut view = view(FakeRepository::default(), LeaderboardTab::Balance, 100);
    view.switch_tab(LeaderboardTab::Gambling);
    assert_eq!(
        view.button_style(LeaderboardTab::Balance),
        ButtonStyle::Secondary
    );
    assert_eq!(
        view.button_style(LeaderboardTab::Gambling),
        ButtonStyle::Primary
    );
    assert_eq!(
        view.button_style(LeaderboardTab::Glicko),
        ButtonStyle::Secondary
    );
}

#[test]
fn test_tabs_have_independent_pages() {
    let mut view = view(FakeRepository::default(), LeaderboardTab::Balance, 100);
    view.tab_state_mut(LeaderboardTab::Balance).current_page = 2;
    view.tab_state_mut(LeaderboardTab::Gambling).current_page = 0;
    view.tab_state_mut(LeaderboardTab::Glicko).current_page = 5;
    assert_eq!(view.tab_state(LeaderboardTab::Balance).current_page, 2);
    assert_eq!(view.tab_state(LeaderboardTab::Gambling).current_page, 0);
    assert_eq!(view.tab_state(LeaderboardTab::Glicko).current_page, 5);
}

#[test]
fn test_pagination_buttons_reflect_current_tab() {
    let mut view = view(FakeRepository::default(), LeaderboardTab::Balance, 100);
    {
        let state = view.tab_state_mut(LeaderboardTab::Balance);
        state.current_page = 0;
        state.max_page = 3;
        state.loaded = true;
    }
    assert_eq!(
        view.pagination_buttons(),
        PaginationButtons {
            previous_disabled: true,
            next_disabled: false
        }
    );
    assert!(view.next_page());
    view.tab_state_mut(LeaderboardTab::Balance).current_page = 3;
    assert_eq!(
        view.pagination_buttons(),
        PaginationButtons {
            previous_disabled: false,
            next_disabled: true
        }
    );
    assert!(!view.next_page());
}

#[test]
fn test_data_not_loaded_initially() {
    let view = view(FakeRepository::default(), LeaderboardTab::Balance, 100);
    assert!(
        LeaderboardTab::ALL
            .into_iter()
            .all(|tab| !view.tab_state(tab).loaded)
    );
}

#[test]
fn test_load_marks_tab_as_loaded() {
    let mut view = view(FakeRepository::default(), LeaderboardTab::Balance, 100);
    assert!(view.load_tab(LeaderboardTab::Balance).unwrap());
    assert!(view.tab_state(LeaderboardTab::Balance).loaded);
    assert!(!view.tab_state(LeaderboardTab::Gambling).loaded);
}

#[test]
fn test_second_load_does_not_refetch() {
    let mut view = view(FakeRepository::default(), LeaderboardTab::Balance, 100);
    assert!(view.load_tab(LeaderboardTab::Balance).unwrap());
    let call_count = view.repository().calls.len();
    assert!(!view.load_tab(LeaderboardTab::Balance).unwrap());
    assert_eq!(view.repository().calls.len(), call_count);
}

#[test]
fn test_balance_backfills_after_departed_member() {
    let repository = FakeRepository {
        balance: vec![
            player(789, GUILD + 1, 10_000),
            player(999, GUILD, 300),
            player(456, GUILD, 200),
            player(123, GUILD, 200),
        ],
        player_count: 4,
        ..FakeRepository::default()
    };
    let mut view = view(repository, LeaderboardTab::Balance, 2);
    view.load_tab(LeaderboardTab::Balance).unwrap();
    let Some(TabData::Balance(data)) = &view.tab_state(LeaderboardTab::Balance).data else {
        panic!("balance data");
    };
    assert_eq!(
        data.players
            .iter()
            .map(|entry| entry.discord_id)
            .collect::<Vec<_>>(),
        [123, 456]
    );
    assert_eq!(data.total_count, 2);
    assert_eq!(data.players[0].rank, 1);
    assert!(
        view.repository()
            .calls
            .contains(&RepositoryCall::BalanceCandidates {
                guild_id: Some(GUILD),
                limit: ALL_LEADERBOARD_ENTRIES_LIMIT
            })
    );
}

fn assert_rating_backfill(kind: RatingKind) {
    let candidates = vec![
        player(789, GUILD + 1, 10_000),
        player(999, GUILD, 300),
        player(456, GUILD, 200),
        player(123, GUILD, 200),
    ];
    let repository = match kind {
        RatingKind::Glicko => FakeRepository {
            glicko: candidates,
            glicko_count: 4,
            ..FakeRepository::default()
        },
        RatingKind::OpenSkill => FakeRepository {
            openskill: candidates,
            openskill_count: 4,
            ..FakeRepository::default()
        },
    };
    let tab = match kind {
        RatingKind::Glicko => LeaderboardTab::Glicko,
        RatingKind::OpenSkill => LeaderboardTab::OpenSkill,
    };
    let mut view = view(repository, tab, 2);
    view.load_tab(tab).unwrap();
    let data = match &view.tab_state(tab).data {
        Some(TabData::Glicko(data) | TabData::OpenSkill(data)) => data,
        _ => panic!("rating data"),
    };
    assert_eq!(
        data.players
            .iter()
            .map(|entry| entry.discord_id)
            .collect::<Vec<_>>(),
        [123, 456]
    );
    assert_eq!(data.total_rated, 2);
    assert!(
        view.repository()
            .calls
            .contains(&RepositoryCall::RatingCandidates {
                guild_id: Some(GUILD),
                kind,
                limit: ALL_LEADERBOARD_ENTRIES_LIMIT
            })
    );
}

#[test]
fn test_rating_tabs_backfill_after_departed_member_glicko() {
    assert_rating_backfill(RatingKind::Glicko);
}

#[test]
fn test_rating_tabs_backfill_after_departed_member_openskill() {
    assert_rating_backfill(RatingKind::OpenSkill);
}

#[test]
fn test_gambling_backfills_after_departed_member() {
    let repository = FakeRepository {
        gambling: Some(GamblingSnapshot {
            entries: vec![
                gambling_entry(789, GUILD + 1, 10_000),
                gambling_entry(999, GUILD, 300),
                gambling_entry(456, GUILD, 200),
                gambling_entry(123, GUILD, 200),
            ],
            server_stats: None,
        }),
        ..FakeRepository::default()
    };
    let mut view = view(repository, LeaderboardTab::Gambling, 2);
    view.load_tab(LeaderboardTab::Gambling).unwrap();
    let Some(TabData::Gambling(data)) = &view.tab_state(LeaderboardTab::Gambling).data else {
        panic!("gambling data");
    };
    assert_eq!(
        data.top_earners
            .iter()
            .map(|entry| entry.discord_id)
            .collect::<Vec<_>>(),
        [123, 456]
    );
    assert!(
        view.repository()
            .calls
            .contains(&RepositoryCall::GamblingSnapshot {
                guild_id: Some(GUILD),
                limit: ALL_LEADERBOARD_ENTRIES_LIMIT
            })
    );
}

#[test]
fn test_tips_backfills_after_departed_member() {
    let repository = FakeRepository {
        tips: Some(TipsSnapshot {
            top_senders: vec![
                tip_entry(789, GUILD + 1, 10_000),
                tip_entry(999, GUILD, 300),
                tip_entry(456, GUILD, 200),
                tip_entry(123, GUILD, 200),
            ],
            top_receivers: Vec::new(),
            total_volume: TipVolume::default(),
        }),
        ..FakeRepository::default()
    };
    let mut view = view(repository, LeaderboardTab::Tips, 2);
    view.load_tab(LeaderboardTab::Tips).unwrap();
    let Some(TabData::Tips(data)) = &view.tab_state(LeaderboardTab::Tips).data else {
        panic!("tips data");
    };
    assert_eq!(
        data.top_senders
            .iter()
            .map(|entry| entry.discord_id)
            .collect::<Vec<_>>(),
        [123, 456]
    );
    assert!(
        view.repository()
            .calls
            .contains(&RepositoryCall::TipsSnapshot {
                guild_id: Some(GUILD),
                limit: ALL_LEADERBOARD_ENTRIES_LIMIT
            })
    );
}

#[test]
fn test_trivia_backfills_after_departed_member() {
    let repository = FakeRepository {
        trivia: vec![
            TriviaEntry {
                discord_id: 789,
                guild_id: Some(GUILD + 1),
                best_streak: 100,
            },
            TriviaEntry {
                discord_id: 999,
                guild_id: Some(GUILD),
                best_streak: 10,
            },
            TriviaEntry {
                discord_id: 456,
                guild_id: Some(GUILD),
                best_streak: 9,
            },
            TriviaEntry {
                discord_id: 123,
                guild_id: Some(GUILD),
                best_streak: 9,
            },
        ],
        ..FakeRepository::default()
    };
    let mut view = view(repository, LeaderboardTab::Trivia, 2);
    view.load_tab(LeaderboardTab::Trivia).unwrap();
    let Some(TabData::Trivia(data)) = &view.tab_state(LeaderboardTab::Trivia).data else {
        panic!("trivia data");
    };
    assert_eq!(
        data.iter()
            .map(|entry| entry.discord_id)
            .collect::<Vec<_>>(),
        [123, 456]
    );
    assert!(
        view.repository()
            .calls
            .contains(&RepositoryCall::TriviaCandidates {
                guild_id: Some(GUILD),
                days: TRIVIA_WINDOW_DAYS,
                limit: ALL_LEADERBOARD_ENTRIES_LIMIT
            })
    );
}

#[test]
fn test_build_balance_embed_empty() {
    let mut view = view(FakeRepository::default(), LeaderboardTab::Balance, 100);
    view.load_tab(LeaderboardTab::Balance).unwrap();
    let embed = view.build_embed();
    assert!(title(&embed).contains("Balance"));
    assert_eq!(description(&embed), "No players registered yet!");
}

#[test]
fn test_build_gambling_embed_empty() {
    let mut view = view(FakeRepository::default(), LeaderboardTab::Gambling, 100);
    view.load_tab(LeaderboardTab::Gambling).unwrap();
    let embed = view.build_embed();
    assert!(title(&embed).contains("Gambling"));
    assert!(description(&embed).contains("No gambling data yet"));
}

#[test]
fn test_build_gambling_embed_unavailable_service() {
    let repository = FakeRepository {
        gambling: None,
        ..FakeRepository::default()
    };
    let mut view = view(repository, LeaderboardTab::Gambling, 100);
    view.load_tab(LeaderboardTab::Gambling).unwrap();
    let embed = view.build_embed();
    assert!(title(&embed).contains("Gambling"));
    assert!(description(&embed).contains("Gambling stats service is not available"));
}

#[test]
fn test_gambling_bankruptcy_lookup_scoped_to_guild() {
    let repository = FakeRepository {
        gambling: Some(GamblingSnapshot {
            entries: vec![gambling_entry(123, GUILD, 100)],
            server_stats: None,
        }),
        bankruptcies: BTreeMap::from([(
            123,
            BankruptcyState {
                penalty_games_remaining: 2,
            },
        )]),
        ..FakeRepository::default()
    };
    let mut view = view(repository, LeaderboardTab::Gambling, 100);
    view.load_tab(LeaderboardTab::Gambling).unwrap();
    assert!(view.repository().calls.iter().any(|call| matches!(
        call,
        RepositoryCall::BankruptcyStates {
            guild_id: Some(GUILD),
            discord_ids
        } if discord_ids == &[123]
    )));
    assert!(
        view.build_embed()
            .fields
            .iter()
            .any(|field| field.value.contains("🪦 User123"))
    );
}

#[test]
fn test_build_glicko_embed_empty() {
    let mut view = view(FakeRepository::default(), LeaderboardTab::Glicko, 100);
    view.load_tab(LeaderboardTab::Glicko).unwrap();
    let embed = view.build_embed();
    assert!(title(&embed).contains("Glicko"));
    assert!(description(&embed).contains("No players with Glicko-2 ratings yet"));
}

#[test]
fn test_build_openskill_embed_empty() {
    let mut view = view(FakeRepository::default(), LeaderboardTab::OpenSkill, 100);
    view.load_tab(LeaderboardTab::OpenSkill).unwrap();
    let embed = view.build_embed();
    assert!(title(&embed).contains("OpenSkill"));
    assert!(description(&embed).contains("No players with OpenSkill ratings yet"));
}

#[test]
fn test_single_section_page_size() {
    assert_eq!(SINGLE_SECTION_PAGE_SIZE, 20);
}

#[test]
fn test_multi_section_page_size() {
    assert_eq!(MULTI_SECTION_PAGE_SIZE, 8);
}

fn assert_deep_link(value: &str, expected: LeaderboardTab) {
    let view = UnifiedLeaderboardView::from_type_parameter(
        FakeRepository::default(),
        Some(GUILD),
        Some(member_directory()),
        Some(value),
        100,
    );
    assert_eq!(view.current_tab(), expected);
}

#[test]
fn test_balance_deep_link() {
    assert_deep_link("balance", LeaderboardTab::Balance);
}

#[test]
fn test_gambling_deep_link() {
    assert_deep_link("gambling", LeaderboardTab::Gambling);
}

#[test]
fn test_glicko_deep_link() {
    assert_deep_link("glicko", LeaderboardTab::Glicko);
}

#[test]
fn test_openskill_deep_link() {
    assert_deep_link("openskill", LeaderboardTab::OpenSkill);
}

#[test]
fn test_tips_deep_link() {
    assert_deep_link("tips", LeaderboardTab::Tips);
}

#[test]
fn test_build_tips_embed_empty() {
    let repository = FakeRepository {
        tips: Some(TipsSnapshot::default()),
        ..FakeRepository::default()
    };
    let mut view = view(repository, LeaderboardTab::Tips, 100);
    view.load_tab(LeaderboardTab::Tips).unwrap();
    let embed = view.build_embed();
    assert!(title(&embed).contains("Tips"));
    assert!(description(&embed).contains("No tips yet"));
}

#[test]
fn test_build_tips_embed_with_data() {
    let repository = FakeRepository {
        tips: Some(TipsSnapshot {
            top_senders: vec![tip_entry(123, GUILD, 100), tip_entry(456, GUILD, 50)],
            top_receivers: vec![tip_entry(789, GUILD, 80)],
            total_volume: TipVolume {
                total_amount: 150,
                total_fees: 15,
                total_transactions: 8,
            },
        }),
        ..FakeRepository::default()
    };
    let mut view = view(repository, LeaderboardTab::Tips, 100);
    view.load_tab(LeaderboardTab::Tips).unwrap();
    let embed = view.build_embed();
    assert!(title(&embed).contains("Tips"));
    assert_eq!(
        embed
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["💝 Most Generous", "⭐ Fan Favorites", "📊 Server Stats"]
    );
}

#[test]
fn test_tips_tab_no_service() {
    let mut view = view(FakeRepository::default(), LeaderboardTab::Tips, 100);
    view.load_tab(LeaderboardTab::Tips).unwrap();
    let embed = view.build_embed();
    assert!(title(&embed).contains("Tips"));
    assert!(description(&embed).contains("not available"));
}

fn worst_case_rating_entry(rank: usize) -> RatingEntry {
    RatingEntry {
        rank,
        discord_id: 123_456_789_012_345_678,
        username: "VeryLongUsername123".to_owned(),
        rating_display: 3_000,
        certainty: 100.0,
        wins: 99,
        losses: 99,
    }
}

fn worst_case_rating_line(rank: usize) -> String {
    format_rating_entry(&worst_case_rating_entry(rank), "<@123456789012345678>")
}

fn old_rating_line(rank: usize) -> String {
    format!(
        "{} **<@123456789012345678>** - 3000 (100% certain) • 99-99",
        rank_marker(rank)
    )
}

#[test]
fn test_single_entry_length() {
    let entry = worst_case_rating_line(1);
    assert!(entry.chars().count() < 50, "entry was {entry:?}");
}

#[test]
fn test_20_entries_under_1024_chars() {
    let content = (1..=SINGLE_SECTION_PAGE_SIZE)
        .map(worst_case_rating_line)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(content.chars().count() <= FIELD_VALUE_LIMIT);
}

#[test]
fn test_new_format_is_shorter_than_old() {
    let old_length = old_rating_line(1).chars().count();
    let new_length = worst_case_rating_line(1).chars().count();
    assert!(old_length.saturating_sub(new_length) >= 12);
}

#[test]
fn test_old_format_would_exceed_limit() {
    let content = (1..=SINGLE_SECTION_PAGE_SIZE)
        .map(old_rating_line)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(content.chars().count() > FIELD_VALUE_LIMIT);
}

#[test]
fn test_openskill_20_entries_under_1024_chars() {
    let content = (1..=SINGLE_SECTION_PAGE_SIZE)
        .map(worst_case_rating_line)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(content.chars().count() <= FIELD_VALUE_LIMIT);
}

#[test]
fn test_short_username_fallback() {
    let entry = format_rating_entry(&worst_case_rating_entry(1), "VeryLongUsername123");
    assert!(entry.chars().count() < 50);
}

#[test]
fn test_low_rank_numbers() {
    for rank in [1, 10, 20] {
        assert!(worst_case_rating_line(rank).chars().count() < 50);
    }
}

#[test]
fn test_single_digit_win_loss() {
    let mut entry = worst_case_rating_entry(1);
    entry.rating_display = 1_500;
    entry.certainty = 50.0;
    entry.wins = 1;
    entry.losses = 0;
    let line = format_rating_entry(&entry, "<@123456789012345678>");
    assert!(line.chars().count() < 45);
}

#[test]
fn test_page_calculation_with_many_entries() {
    let view = gambling_view(paged_gambling_entries(20), None, None);
    assert_eq!(
        view.tab_state(LeaderboardTab::Gambling).max_page,
        (20 - 1) / MULTI_SECTION_PAGE_SIZE
    );
}

#[test]
fn test_page_calculation_with_few_entries() {
    let view = gambling_view(paged_gambling_entries(5), None, None);
    assert_eq!(view.tab_state(LeaderboardTab::Gambling).max_page, 0);
}

#[test]
fn test_build_embed_respects_field_limits() {
    let entries = paged_gambling_entries(20);
    let members = GuildMemberDirectory::new(entries.iter().map(|entry| {
        (
            entry.discord_id,
            format!(
                "{}-{}",
                "ExtremelyLongGamblerDisplayName".repeat(12),
                entry.discord_id
            ),
        )
    }));
    let view = gambling_view(entries, Some(members), None);
    let embed = view.build_embed();
    assert!(!embed.fields.is_empty());
    assert!(
        embed
            .fields
            .iter()
            .all(|field| field.value.chars().count() <= FIELD_VALUE_LIMIT)
    );
}

#[test]
fn test_empty_sections_not_added() {
    let view = gambling_view(Vec::new(), None, None);
    let embed = view.build_embed();
    assert!(embed.fields.is_empty());
    assert!(description(&embed).contains("No gambling data yet"));
}

#[test]
fn test_pagination_buttons_disabled_appropriately() {
    let mut view = gambling_view(paged_gambling_entries(20), None, None);
    assert_eq!(
        view.pagination_buttons(),
        PaginationButtons {
            previous_disabled: true,
            next_disabled: false,
        }
    );
    while view.next_page() {}
    assert_eq!(
        view.pagination_buttons(),
        PaginationButtons {
            previous_disabled: false,
            next_disabled: true,
        }
    );
}

#[test]
fn test_single_page_both_buttons_disabled() {
    let view = gambling_view(paged_gambling_entries(3), None, None);
    assert_eq!(
        view.pagination_buttons(),
        PaginationButtons {
            previous_disabled: true,
            next_disabled: true,
        }
    );
}

#[test]
fn test_page_slicing_shows_correct_entries() {
    let mut view = gambling_view(paged_gambling_entries(20), None, None);
    let first = view.build_embed();
    let first_earners = first
        .fields
        .iter()
        .find(|field| field.name == "Top Earners")
        .expect("first-page earners");
    assert!(first_earners.value.contains("User 1000"));
    assert!(first_earners.value.contains("User 1007"));
    assert!(!first_earners.value.contains("User 1008"));

    assert!(view.next_page());
    let second = view.build_embed();
    let second_earners = second
        .fields
        .iter()
        .find(|field| field.name == "Top Earners")
        .expect("second-page earners");
    assert!(!second_earners.value.contains("User 1000"));
    assert!(second_earners.value.contains("User 1008"));
    assert!(second_earners.value.contains("User 1015"));
}

#[test]
fn test_down_bad_filters_negative_only() {
    let entries = [100, -50, 0, -100]
        .into_iter()
        .enumerate()
        .map(|(index, pnl)| gambling_entry(2_001 + index as i64, GUILD, pnl))
        .collect();
    let view = gambling_view(entries, None, None);
    let embed = view.build_embed();
    let down_bad = embed
        .fields
        .iter()
        .find(|field| field.name == "Down Bad")
        .expect("down-bad field");
    assert!(down_bad.value.contains("User 2002"));
    assert!(down_bad.value.contains("User 2004"));
    assert!(!down_bad.value.contains("User 2001"));
    assert!(!down_bad.value.contains("User 2003"));
}

#[test]
fn test_footer_includes_page_info() {
    let view = gambling_view(
        paged_gambling_entries(20),
        None,
        Some(GamblingServerStats {
            total_bets: 100,
            total_wagered: 10_000,
            unique_gamblers: 20,
            total_bankruptcies: 2,
        }),
    );
    let footer = view.build_embed().footer.expect("gambling footer");
    assert!(footer.contains("Page 1/3"));
    assert!(footer.contains("100 bets"));
}

#[test]
fn test_index_numbering_continues_across_pages() {
    let mut view = gambling_view(paged_gambling_entries(20), None, None);
    let first = view.build_embed();
    let first_earners = first
        .fields
        .iter()
        .find(|field| field.name == "Top Earners")
        .expect("first page");
    assert!(first_earners.value.starts_with("1. **User 1000**"));

    assert!(view.next_page());
    let second = view.build_embed();
    let second_earners = second
        .fields
        .iter()
        .find(|field| field.name == "Top Earners")
        .expect("second page");
    assert!(second_earners.value.starts_with("9. **User 1008**"));
}
