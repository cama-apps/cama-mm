use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::betting_service_repository::{
    BetSettlementAdjustments, BettingServiceRepository, PlaceBetRequest,
};
use crate::dota_bet_seed_repository::{BettingMode, BettingTeam};
use crate::test_support::FastTestDatabase;

const DEFAULT_GUILD: i64 = 0;

static FIXTURE_DATABASE_TEMPLATE: OnceLock<NamedTempFile> = OnceLock::new();

fn auto_history_bet(
    match_id: i64,
    team: BetSide,
    automatic: bool,
    wagered: i64,
    profit: i64,
    target_id: Option<i64>,
    direction: Option<&str>,
) -> BetHistoryEntry {
    BetHistoryEntry {
        bet_id: match_id,
        amount: wagered,
        leverage: 1,
        effective_bet: wagered,
        team,
        is_blind: automatic,
        investment_target_id: target_id,
        investment_direction: direction.map(str::to_owned),
        bet_time: match_id,
        match_id,
        payout: Some(wagered.saturating_add(profit).max(0)),
        vanity_tax: 0,
        low_priority_tax: 0,
        outcome: if profit >= 0 {
            GamblingOutcome::Won
        } else {
            GamblingOutcome::Lost
        },
        profit,
    }
}

struct Fixture {
    file: FastTestDatabase,
    next_pending_id: i64,
    next_match_id: i64,
    next_time: i64,
}

impl Fixture {
    fn new() -> Self {
        let template = FIXTURE_DATABASE_TEMPLATE.get_or_init(|| {
            let file =
                NamedTempFile::new().expect("temporary gambling statistics database template");
            let connection = Connection::open(file.path()).expect("open fixture template");
            connection
                .execute_batch(
                    "CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_username TEXT NOT NULL,
                     jopacoin_balance INTEGER DEFAULT 3,
                     lowest_balance_ever INTEGER,
                     last_double_or_nothing INTEGER,
                     updated_at TEXT,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE pending_matches (
                     pending_match_id INTEGER PRIMARY KEY,
                     guild_id INTEGER NOT NULL,
                     payload TEXT NOT NULL
                 );
                 CREATE TABLE bets (
                     bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     match_id INTEGER,
                     discord_id INTEGER NOT NULL,
                     team_bet_on TEXT NOT NULL,
                     amount INTEGER NOT NULL,
                     bet_time INTEGER NOT NULL,
                     leverage INTEGER DEFAULT 1,
                     payout INTEGER,
                     is_blind INTEGER DEFAULT 0,
                     odds_at_placement REAL,
                     pending_match_id INTEGER,
                     investment_target_id INTEGER,
                     investment_direction TEXT
                 );
                 CREATE TABLE matches (
                     match_id INTEGER PRIMARY KEY,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     team1_players TEXT NOT NULL DEFAULT '[]',
                     team2_players TEXT NOT NULL DEFAULT '[]',
                     winning_team INTEGER
                 );
                 CREATE TABLE match_participants (
                     match_id INTEGER NOT NULL,
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     team_number INTEGER NOT NULL,
                     won INTEGER,
                     side TEXT,
                     PRIMARY KEY (match_id, discord_id)
                 );
                 CREATE TABLE bet_settlement_taxes (
                     match_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_id INTEGER NOT NULL,
                     vanity_tax INTEGER NOT NULL DEFAULT 0,
                     low_priority_tax INTEGER NOT NULL DEFAULT 0,
                     PRIMARY KEY (match_id, guild_id, discord_id)
                 );
                 CREATE TABLE bankruptcy_state (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     last_bankruptcy_at INTEGER,
                     penalty_games_remaining INTEGER DEFAULT 0,
                     bankruptcy_count INTEGER DEFAULT 0,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE loan_state (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     total_loans_taken INTEGER DEFAULT 0,
                     negative_loans_taken INTEGER DEFAULT 0,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE wheel_spins (
                     spin_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_id INTEGER NOT NULL,
                     result INTEGER NOT NULL,
                     spin_time INTEGER NOT NULL
                 );
                 CREATE TABLE double_or_nothing_spins (
                     spin_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_id INTEGER NOT NULL,
                     cost INTEGER NOT NULL,
                     balance_before INTEGER NOT NULL,
                     balance_after INTEGER NOT NULL,
                     won INTEGER NOT NULL,
                     spin_time INTEGER NOT NULL
                 );
                 CREATE TABLE economy_ledger_context (
                     id INTEGER PRIMARY KEY CHECK (id = 1),
                     source TEXT,
                     actor_id INTEGER,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );",
                )
                .expect("create disposable schema template");
            drop(connection);
            file
        });
        let file = FastTestDatabase::from_template(template.path());
        Self {
            file,
            next_pending_id: 1,
            next_match_id: 1,
            next_time: 1_000,
        }
    }

    fn path(&self) -> &Path {
        self.file.path()
    }

    fn stats_repository(&self) -> GamblingStatsRepository {
        GamblingStatsRepository::new(self.path())
    }

    fn service(&self) -> GamblingStatsService<GamblingStatsRepository> {
        GamblingStatsService::new(self.stats_repository())
    }

    fn add_player(&self, discord_id: i64, balance: i64, guild_id: i64) {
        Connection::open(self.path())
            .expect("open fixture")
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance,
                     lowest_balance_ever
                 ) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![
                    discord_id,
                    guild_id,
                    format!("TestPlayer{discord_id}"),
                    balance
                ],
            )
            .expect("add player");
    }

    fn settle_bet(
        &mut self,
        discord_id: i64,
        amount: i64,
        leverage: i64,
        winner: BettingTeam,
        guild_id: i64,
    ) -> i64 {
        let pending_id = self.next_pending_id;
        self.next_pending_id += 1;
        let match_id = self.next_match_id;
        self.next_match_id += 1;
        let bet_time = self.next_time;
        self.next_time += 1;
        let payload = format!(
            "{{\"radiant_team_ids\":[{discord_id}],\"dire_team_ids\":[999],\"bet_lock_until\":{},\"is_draft\":false}}",
            bet_time + 1_000
        );
        let connection = Connection::open(self.path()).expect("open fixture");
        connection
            .execute(
                "INSERT INTO pending_matches (pending_match_id, guild_id, payload)
                 VALUES (?1, ?2, ?3)",
                params![pending_id, guild_id, payload],
            )
            .expect("insert pending match");
        drop(connection);
        let betting = BettingServiceRepository::new(self.path());
        betting
            .place_bet_atomic(PlaceBetRequest {
                guild_id: Some(guild_id),
                pending_match_id: pending_id,
                discord_id,
                team: BettingTeam::Radiant,
                amount,
                bet_time,
                leverage,
                max_debt: 500,
                is_blind: false,
                odds_at_placement: None,
            })
            .expect("place bet through production adapter");
        let connection = Connection::open(self.path()).expect("open fixture");
        connection
            .execute(
                "INSERT INTO matches (
                     match_id, guild_id, team1_players, team2_players, winning_team
                 ) VALUES (?1, ?2, ?3, '[999]', ?4)",
                params![
                    match_id,
                    guild_id,
                    format!("[{discord_id}]"),
                    if winner == BettingTeam::Radiant { 1 } else { 2 }
                ],
            )
            .expect("record match");
        connection
            .execute(
                "INSERT INTO match_participants (
                     match_id, discord_id, guild_id, team_number, won, side
                 ) VALUES (?1, ?2, ?3, 1, ?4, 'radiant')",
                params![
                    match_id,
                    discord_id,
                    guild_id,
                    i64::from(winner == BettingTeam::Radiant)
                ],
            )
            .expect("record participant");
        drop(connection);
        betting
            .settle_pending_bets_atomic(
                match_id,
                Some(guild_id),
                bet_time - 100,
                Some(pending_id),
                winner,
                BettingMode::House,
            )
            .expect("settle bet through production adapter");
        match_id
    }

    fn record_match_without_bet(&mut self, discord_id: i64, guild_id: i64) -> i64 {
        let match_id = self.next_match_id;
        self.next_match_id += 1;
        let connection = Connection::open(self.path()).expect("open fixture");
        connection
            .execute(
                "INSERT INTO matches (
                     match_id, guild_id, team1_players, team2_players, winning_team
                 ) VALUES (?1, ?2, ?3, '[999]', 1)",
                params![match_id, guild_id, format!("[{discord_id}]")],
            )
            .expect("record match");
        connection
            .execute(
                "INSERT INTO match_participants (
                     match_id, discord_id, guild_id, team_number, won, side
                 ) VALUES (?1, ?2, ?3, 1, 1, 'radiant')",
                params![match_id, discord_id, guild_id],
            )
            .expect("record participant");
        match_id
    }
}

type MetricCall = (Option<i64>, Option<Vec<i64>>);

#[derive(Clone)]
struct CountingPort {
    inner: GamblingStatsRepository,
    history_calls: Cell<usize>,
    metric_calls: RefCell<Vec<MetricCall>>,
}

impl CountingPort {
    fn new(inner: GamblingStatsRepository) -> Self {
        Self {
            inner,
            history_calls: Cell::new(0),
            metric_calls: RefCell::new(Vec::new()),
        }
    }
}

impl GamblingStatsPort for CountingPort {
    fn betting_impact_bets(
        &self,
        target_discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<BettingImpactBet>, GamblingStatsError> {
        self.inner.betting_impact_bets(target_discord_id, guild_id)
    }

    fn player_bet_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<BetHistoryEntry>, GamblingStatsError> {
        self.history_calls.set(self.history_calls.get() + 1);
        self.inner.player_bet_history(discord_id, guild_id)
    }

    fn paper_hands(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<PaperHands, GamblingStatsError> {
        self.inner.paper_hands(discord_id, guild_id)
    }

    fn player_bankruptcy_count(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, GamblingStatsError> {
        self.inner.player_bankruptcy_count(discord_id, guild_id)
    }

    fn bulk_bankruptcy_counts(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, GamblingStatsError> {
        self.inner.bulk_bankruptcy_counts(discord_ids, guild_id)
    }

    fn total_settled_matches(&self, guild_id: Option<i64>) -> Result<i64, GamblingStatsError> {
        self.inner.total_settled_matches(guild_id)
    }

    fn lowest_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, GamblingStatsError> {
        self.inner.lowest_balance(discord_id, guild_id)
    }

    fn lowest_balances_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, Option<i64>>, GamblingStatsError> {
        self.inner.lowest_balances_bulk(discord_ids, guild_id)
    }

    fn negative_loans_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, GamblingStatsError> {
        self.inner.negative_loans_bulk(discord_ids, guild_id)
    }

    fn total_loans_taken(&self, guild_id: Option<i64>) -> Result<i64, GamblingStatsError> {
        self.inner.total_loans_taken(guild_id)
    }

    fn bulk_gambling_metrics(
        &self,
        guild_id: Option<i64>,
        discord_ids: Option<&[i64]>,
    ) -> Result<BTreeMap<i64, GamblingMetric>, GamblingStatsError> {
        self.metric_calls
            .borrow_mut()
            .push((guild_id, discord_ids.map(<[i64]>::to_vec)));
        self.inner.bulk_gambling_metrics(guild_id, discord_ids)
    }

    fn wheel_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<WheelSpin>, GamblingStatsError> {
        self.inner.wheel_history(discord_id, guild_id)
    }

    fn double_or_nothing_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<DoubleOrNothingSpin>, GamblingStatsError> {
        self.inner.double_or_nothing_history(discord_id, guild_id)
    }
}

fn close(left: f64, right: f64) {
    assert!((left - right).abs() < 1.0e-9, "{left} != {right}");
}

#[test]
fn test_get_player_bet_history_with_bets() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 100, DEFAULT_GUILD);
    let repository = fixture.stats_repository();
    assert!(
        repository
            .player_bet_history(1001, None)
            .expect("empty history")
            .is_empty()
    );
    fixture.settle_bet(1001, 10, 1, BettingTeam::Radiant, DEFAULT_GUILD);
    fixture.settle_bet(1001, 15, 1, BettingTeam::Dire, DEFAULT_GUILD);
    fixture.settle_bet(1001, 12, 2, BettingTeam::Radiant, DEFAULT_GUILD);
    let history = repository
        .player_bet_history(1001, None)
        .expect("settled history");
    assert_eq!(history.len(), 3);
    let by_amount = history
        .iter()
        .map(|row| (row.amount, row))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(by_amount[&10].outcome, GamblingOutcome::Won);
    assert_eq!(by_amount[&10].profit, 10);
    assert_eq!(by_amount[&10].leverage, 1);
    assert_eq!(by_amount[&15].outcome, GamblingOutcome::Lost);
    assert_eq!(by_amount[&15].profit, -15);
    assert_eq!(by_amount[&12].leverage, 2);
    assert_eq!(by_amount[&12].effective_bet, 24);
    assert_eq!(by_amount[&12].profit, 24);
}

#[test]
fn test_real_investment_bet_persists_attribution_and_uses_settled_pnl() {
    let fixture = Fixture::new();
    let investor_id = 101;
    let target_id = 201;
    let opponent_id = 202;
    for discord_id in [investor_id, target_id, opponent_id] {
        fixture.add_player(discord_id, 100, DEFAULT_GUILD);
    }
    let pending_id = 7;
    let match_id = 70;
    let shuffle_timestamp = 1_700_000_000;
    Connection::open(fixture.path())
        .expect("open investment fixture")
        .execute(
            "INSERT INTO pending_matches (pending_match_id, guild_id, payload)
             VALUES (?1, ?2, ?3)",
            params![
                pending_id,
                DEFAULT_GUILD,
                format!(
                    "{{\"radiant_team_ids\":[{target_id}],\"dire_team_ids\":[{opponent_id}],\"bet_lock_until\":{},\"is_draft\":false}}",
                    shuffle_timestamp + 1_000
                )
            ],
        )
        .expect("insert investment pending match");
    let betting = BettingServiceRepository::new(fixture.path());
    betting
        .place_investment_bet_atomic(
            PlaceBetRequest {
                guild_id: Some(DEFAULT_GUILD),
                pending_match_id: pending_id,
                discord_id: investor_id,
                team: BettingTeam::Radiant,
                amount: 10,
                bet_time: shuffle_timestamp,
                leverage: 1,
                max_debt: 500,
                is_blind: true,
                odds_at_placement: None,
            },
            target_id,
            "long",
        )
        .expect("place attributed investment");
    Connection::open(fixture.path())
        .expect("open settled investment fixture")
        .execute(
            "INSERT INTO matches (
                 match_id, guild_id, team1_players, team2_players, winning_team
             ) VALUES (?1, ?2, ?3, ?4, 1)",
            params![
                match_id,
                DEFAULT_GUILD,
                format!("[{target_id}]"),
                format!("[{opponent_id}]")
            ],
        )
        .expect("record investment match");
    betting
        .settle_pending_bets_atomic(
            match_id,
            Some(DEFAULT_GUILD),
            shuffle_timestamp,
            Some(pending_id),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .expect("settle attributed investment");

    let history = fixture
        .stats_repository()
        .player_bet_history(investor_id, Some(DEFAULT_GUILD))
        .expect("settled investment history");
    assert_eq!(history.len(), 1);
    assert!(history[0].is_blind);
    assert_eq!(history[0].investment_target_id, Some(target_id));
    assert_eq!(history[0].investment_direction.as_deref(), Some("long"));
    assert_eq!(history[0].effective_bet, 10);
    assert_eq!(history[0].profit, 10);

    let stats = fixture
        .service()
        .get_player_stats(investor_id, Some(DEFAULT_GUILD))
        .expect("investment profile stats")
        .expect("investor stats");
    assert_eq!(stats.auto_bet_performance.total.net_pnl, 10);
    assert_eq!(stats.auto_bet_performance.generic.bet_count, 0);
    assert_eq!(stats.auto_bet_performance.targets.len(), 1);
    assert_eq!(stats.auto_bet_performance.targets[0].target_id, target_id);
    assert_eq!(stats.auto_bet_performance.targets[0].directions, ["long"]);
    assert_eq!(stats.auto_bet_performance.targets[0].net_pnl, 10);
}

#[test]
fn test_betting_impact_aggregates_external_settled_bets_and_profiles() {
    let fixture = Fixture::new();
    let target_id = 2_001;
    let supporters = [2_101, 2_102];
    let haters = [2_103, 2_104];
    for discord_id in [target_id, 999] {
        fixture.add_player(discord_id, 100, DEFAULT_GUILD);
    }
    for discord_id in supporters.into_iter().chain(haters) {
        fixture.add_player(discord_id, 100, DEFAULT_GUILD);
    }
    let pending_id = 801;
    let match_id = 901;
    let bet_time = 1_800_000_000;
    Connection::open(fixture.path())
        .expect("open impact fixture")
        .execute(
            "INSERT INTO pending_matches (pending_match_id, guild_id, payload)
             VALUES (?1, ?2, ?3)",
            params![
                pending_id,
                DEFAULT_GUILD,
                format!(
                    "{{\"radiant_team_ids\":[{target_id}],\"dire_team_ids\":[999],\"bet_lock_until\":{},\"is_draft\":false}}",
                    bet_time + 1_000
                )
            ],
        )
        .expect("insert impact pending match");
    let betting = BettingServiceRepository::new(fixture.path());
    for (discord_id, team, amount) in [
        (supporters[0], BettingTeam::Radiant, 45),
        (supporters[1], BettingTeam::Radiant, 30),
        (haters[0], BettingTeam::Dire, 15),
        (haters[1], BettingTeam::Dire, 10),
    ] {
        betting
            .place_bet_atomic(PlaceBetRequest {
                guild_id: Some(DEFAULT_GUILD),
                pending_match_id: pending_id,
                discord_id,
                team,
                amount,
                bet_time,
                leverage: 1,
                max_debt: 500,
                is_blind: false,
                odds_at_placement: None,
            })
            .expect("place impact wager");
    }
    let connection = Connection::open(fixture.path()).expect("open impact settled fixture");
    connection
        .execute(
            "INSERT INTO matches (
                 match_id, guild_id, team1_players, team2_players, winning_team
             ) VALUES (?1, ?2, ?3, '[999]', 1)",
            params![match_id, DEFAULT_GUILD, format!("[{target_id}]")],
        )
        .expect("record impact match");
    connection
        .execute(
            "INSERT INTO match_participants (
                 match_id, discord_id, guild_id, team_number, won, side
             ) VALUES (?1, ?2, ?3, 1, 1, 'radiant')",
            params![match_id, target_id, DEFAULT_GUILD],
        )
        .expect("record impact participant");
    betting
        .settle_pending_bets_atomic(
            match_id,
            Some(DEFAULT_GUILD),
            bet_time,
            Some(pending_id),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .expect("settle impact wagers");

    let impact = fixture
        .service()
        .get_betting_impact_stats(target_id, Some(DEFAULT_GUILD))
        .expect("impact stats")
        .expect("external bets");
    assert_eq!(impact.matches_with_bets, 1);
    assert_eq!(impact.total_bets, 4);
    assert_eq!(impact.total_wagered_for, 75);
    assert_eq!(impact.total_wagered_against, 25);
    assert_eq!(impact.supporters_net_pnl, 75);
    assert_eq!(impact.haters_net_pnl, -25);
    assert_eq!(impact.market_favorability, 0.75);
    assert_eq!(impact.supporter_win_rate, 1.0);
    assert_eq!(impact.hater_win_rate, 0.0);
    assert_eq!(
        impact
            .biggest_fan
            .as_ref()
            .map(|profile| profile.discord_id),
        Some(supporters[0])
    );
    assert_eq!(
        impact
            .biggest_hater
            .as_ref()
            .map(|profile| profile.discord_id),
        Some(haters[0])
    );
    assert_eq!(impact.biggest_single_win, 45);
    assert_eq!(impact.biggest_single_loss, -15);
}

#[test]
fn test_betting_impact_excludes_self_unsettled_and_other_guild_bets() {
    let fixture = Fixture::new();
    fixture.add_player(3_001, 100, DEFAULT_GUILD);
    fixture.add_player(3_002, 100, DEFAULT_GUILD);
    fixture.add_player(3_001, 100, 42);
    fixture.add_player(3_002, 100, 42);
    let connection = Connection::open(fixture.path()).expect("open impact isolation fixture");
    connection
        .execute_batch(
            "INSERT INTO matches (
                 match_id, guild_id, team1_players, team2_players, winning_team
             ) VALUES (1, 0, '[3001]', '[3002]', 1),
                      (2, 0, '[3001]', '[3002]', NULL),
                      (3, 42, '[3001]', '[3002]', 1);",
        )
        .expect("insert impact matches");
    connection
        .execute_batch(
            "INSERT INTO match_participants
                 (match_id, discord_id, guild_id, team_number, won, side)
             VALUES (1, 3001, 0, 1, 1, 'radiant'),
                    (2, 3001, 0, 1, NULL, 'radiant'),
                    (3, 3001, 42, 1, 1, 'radiant');
             INSERT INTO bets
                 (guild_id, match_id, discord_id, team_bet_on, amount, bet_time, leverage, payout)
             VALUES (0, 1, 3001, 'radiant', 5, 1, 1, 10),
                    (0, 1, 3002, 'radiant', 7, 1, 1, 14),
                    (0, 2, 3002, 'radiant', 9, 1, 1, NULL),
                    (42, 3, 3002, 'radiant', 11, 1, 1, 22);",
        )
        .expect("insert impact isolation bets");
    let rows = fixture
        .stats_repository()
        .betting_impact_bets(3_001, Some(DEFAULT_GUILD))
        .expect("impact isolation rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].bettor_id, 3_002);
    assert_eq!(rows[0].match_id, 1);
}

fn impact_bet(
    bettor_id: i64,
    match_id: i64,
    effective_bet: i64,
    bet_for_target: bool,
    won: bool,
    profit: i64,
) -> BettingImpactBet {
    BettingImpactBet {
        bettor_id,
        match_id,
        effective_bet,
        payout: Some(effective_bet.saturating_add(profit)),
        vanity_tax: 0,
        low_priority_tax: 0,
        bet_for_target,
        won,
        profit,
    }
}

#[test]
fn test_betting_impact_query_returns_empty_for_player_without_matches() {
    let fixture = Fixture::new();
    fixture.add_player(4_001, 100, DEFAULT_GUILD);
    assert!(
        fixture
            .stats_repository()
            .betting_impact_bets(4_001, Some(DEFAULT_GUILD))
            .expect("empty impact query")
            .is_empty()
    );
}

#[test]
fn test_betting_impact_query_excludes_self_bets() {
    let fixture = Fixture::new();
    fixture.add_player(4_101, 100, DEFAULT_GUILD);
    fixture.add_player(4_102, 100, DEFAULT_GUILD);
    let connection = Connection::open(fixture.path()).expect("open self-bet fixture");
    connection
        .execute_batch(
            "INSERT INTO matches(match_id,guild_id,team1_players,team2_players,winning_team)
             VALUES (1,0,'[4101]','[4102]',1);
             INSERT INTO match_participants
                 (match_id,discord_id,guild_id,team_number,won,side)
             VALUES (1,4101,0,1,1,'radiant');
             INSERT INTO bets
                 (guild_id,match_id,discord_id,team_bet_on,amount,bet_time,leverage,payout)
             VALUES (0,1,4101,'radiant',5,1,1,10),
                    (0,1,4102,'radiant',7,1,1,14);",
        )
        .expect("seed self-bet fixture");
    let rows = fixture
        .stats_repository()
        .betting_impact_bets(4_101, Some(DEFAULT_GUILD))
        .expect("external impact query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].bettor_id, 4_102);
}

#[test]
fn test_betting_impact_query_ignores_unsettled_matches() {
    let fixture = Fixture::new();
    fixture.add_player(4_201, 100, DEFAULT_GUILD);
    fixture.add_player(4_202, 100, DEFAULT_GUILD);
    let connection = Connection::open(fixture.path()).expect("open unsettled fixture");
    connection
        .execute_batch(
            "INSERT INTO matches(match_id,guild_id,team1_players,team2_players,winning_team)
             VALUES (1,0,'[4201]','[4202]',NULL);
             INSERT INTO match_participants
                 (match_id,discord_id,guild_id,team_number,won,side)
             VALUES (1,4201,0,1,NULL,'radiant');
             INSERT INTO bets
                 (guild_id,match_id,discord_id,team_bet_on,amount,bet_time,leverage,payout)
             VALUES (0,1,4202,'radiant',7,1,1,NULL);",
        )
        .expect("seed unsettled fixture");
    assert!(
        fixture
            .stats_repository()
            .betting_impact_bets(4_201, Some(DEFAULT_GUILD))
            .expect("settled-only impact query")
            .is_empty()
    );
}

#[test]
fn test_betting_impact_returns_none_without_external_bets() {
    assert!(compute_betting_impact_stats(4_301, &[]).is_none());
}

#[test]
fn test_betting_impact_blessing_requires_positive_pnl() {
    let bets = [impact_bet(4_401, 1, 10, true, false, -10)];
    let impact = compute_betting_impact_stats(4_499, &bets).expect("impact");
    assert!(impact.blessing.is_none());
    assert!(impact.jinx.is_some());
}

#[test]
fn test_betting_impact_consistent_fan_uses_for_count() {
    let bets = [
        impact_bet(4_501, 1, 5, true, true, 5),
        impact_bet(4_502, 1, 5, true, true, 5),
        impact_bet(4_502, 2, 5, true, true, 5),
    ];
    let impact = compute_betting_impact_stats(4_599, &bets).expect("impact");
    assert_eq!(impact.most_consistent_fan.unwrap().discord_id, 4_502);
}

#[test]
fn test_betting_impact_blessing_and_jinx_track_for_pnl() {
    let bets = [
        impact_bet(4_601, 1, 50, true, true, 50),
        impact_bet(4_601, 2, 10, true, false, -10),
        impact_bet(4_602, 1, 10, true, true, 10),
        impact_bet(4_602, 2, 50, true, false, -50),
    ];
    let impact = compute_betting_impact_stats(4_699, &bets).expect("impact");
    assert_eq!(impact.blessing.unwrap().discord_id, 4_601);
    assert_eq!(impact.jinx.unwrap().discord_id, 4_602);
}

#[test]
fn test_betting_impact_tracks_single_bet_extremes() {
    let bets = [
        impact_bet(4_701, 1, 50, true, true, 50),
        impact_bet(4_702, 1, 30, false, false, -30),
    ];
    let impact = compute_betting_impact_stats(4_799, &bets).expect("impact");
    assert_eq!(impact.biggest_single_win, 50);
    assert_eq!(impact.biggest_single_loss, -30);
}

#[test]
fn test_bet_history_guild_isolation() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 100, 111);
    fixture.add_player(1001, 100, 222);
    fixture.settle_bet(1001, 10, 1, BettingTeam::Radiant, 111);
    let repository = fixture.stats_repository();
    assert_eq!(
        repository
            .player_bet_history(1001, Some(111))
            .expect("guild A")
            .len(),
        1
    );
    assert!(
        repository
            .player_bet_history(1001, Some(222))
            .expect("guild B")
            .is_empty()
    );
}

#[test]
fn test_leverage_distribution_from_history_matches_repository_semantics_empty() {
    assert_eq!(leverage_distribution_from_nullable(&[]), BTreeMap::new());
}

#[test]
fn test_leverage_distribution_from_history_matches_repository_semantics_values() {
    assert_eq!(
        leverage_distribution_from_nullable(&[None, None, Some(1), Some(2), Some(5), Some(5)]),
        BTreeMap::from([(1, 3), (2, 1), (5, 2)])
    );
}

#[test]
fn test_bulk_gambling_metrics_match_legacy_aggregates() {
    let mut fixture = Fixture::new();
    for discord_id in [1001, 1002, 1003] {
        fixture.add_player(discord_id, 1_000, 111);
    }
    fixture.add_player(1001, 1_000, 222);
    fixture.settle_bet(1001, 10, 1, BettingTeam::Dire, 111);
    fixture.settle_bet(1001, 5, 5, BettingTeam::Radiant, 111);
    fixture.settle_bet(1001, 7, 1, BettingTeam::Radiant, 111);
    fixture.settle_bet(1002, 2, 10, BettingTeam::Radiant, 111);
    fixture.settle_bet(1001, 50, 1, BettingTeam::Dire, 222);
    let connection = Connection::open(fixture.path()).expect("open fixture");
    let bet_ids = connection
        .prepare(
            "SELECT bet_id FROM bets WHERE guild_id = 111 AND discord_id = 1001 ORDER BY bet_id",
        )
        .expect("prepare bets")
        .query_map([], |row| row.get::<_, i64>(0))
        .expect("load bets")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect bets");
    connection
        .execute(
            "UPDATE bets SET bet_time = 123 WHERE guild_id = 111 AND discord_id = 1001",
            [],
        )
        .expect("equal timestamps");
    connection
        .execute(
            "UPDATE bets SET leverage = NULL WHERE bet_id = ?1",
            [bet_ids[0]],
        )
        .expect("legacy null leverage");
    connection
        .execute(
            "UPDATE bets SET payout = NULL WHERE bet_id = ?1",
            [bet_ids[1]],
        )
        .expect("missing payout");
    connection
        .execute("UPDATE bets SET payout = 0 WHERE bet_id = ?1", [bet_ids[2]])
        .expect("stored zero payout");
    drop(connection);
    let repository = fixture.stats_repository();
    let metrics = repository
        .bulk_gambling_metrics(Some(111), None)
        .expect("bulk metrics");
    assert_eq!(metrics.keys().copied().collect::<Vec<_>>(), [1001, 1002]);
    let first = metrics[&1001];
    assert_eq!((first.total_bets, first.wins, first.losses), (3, 2, 1));
    assert_eq!(first.total_wagered, 42);
    assert_eq!(first.net_pnl, 8);
    assert_eq!(first.five_x_bets, 1);
    assert_eq!(first.unique_matches, 3);
    assert_eq!(first.sequences_analyzed, 1);
    assert_eq!(first.times_increased_after_loss, 1);
    close(first.win_rate, 2.0 / 3.0);
    close(first.roi, 8.0 / 42.0);
    close(first.avg_leverage, 7.0 / 3.0);
    let history = repository
        .player_bet_history(1001, Some(111))
        .expect("history");
    assert_eq!(
        history.iter().map(|bet| bet.profit).sum::<i64>(),
        first.net_pnl
    );
    assert_eq!(
        history.iter().map(|bet| bet.effective_bet).sum::<i64>(),
        first.total_wagered
    );
    let scoped = repository
        .bulk_gambling_metrics(Some(111), Some(&[1001, 1001, 1003, 9999]))
        .expect("scoped metrics");
    assert_eq!(scoped.keys().copied().collect::<Vec<_>>(), [1001]);
    assert!(
        repository
            .bulk_gambling_metrics(Some(111), Some(&[]))
            .expect("empty scope")
            .is_empty()
    );
}

#[test]
fn test_bankruptcy_debuff_vanity_tax_flows_into_history_and_aggregate_pnl() {
    let fixture = Fixture::new();
    let guild_id = 4_242;
    let discord_id = 1_003;
    fixture.add_player(discord_id, 503, guild_id);
    let connection = Connection::open(fixture.path()).expect("open fixture");
    connection
        .execute_batch(
            "CREATE TABLE economy_ledger_entries (
                 ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
                 guild_id INTEGER NOT NULL,
                 account_type TEXT NOT NULL,
                 account_id INTEGER,
                 delta INTEGER NOT NULL,
                 balance_before INTEGER NOT NULL,
                 balance_after INTEGER NOT NULL,
                 source TEXT NOT NULL
             );
             CREATE TRIGGER ledger_player_update
             AFTER UPDATE OF jopacoin_balance ON players
             WHEN COALESCE(OLD.jopacoin_balance, 0) != COALESCE(NEW.jopacoin_balance, 0)
             BEGIN
                 INSERT INTO economy_ledger_entries (
                     guild_id, account_type, account_id, delta,
                     balance_before, balance_after, source
                 ) VALUES (
                     COALESCE(NEW.guild_id, 0), 'player', NEW.discord_id,
                     COALESCE(NEW.jopacoin_balance, 0) - COALESCE(OLD.jopacoin_balance, 0),
                     COALESCE(OLD.jopacoin_balance, 0), COALESCE(NEW.jopacoin_balance, 0),
                     COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1),
                              'balance_update')
                 );
             END;
             INSERT INTO pending_matches (pending_match_id, guild_id, payload)
             VALUES (700, 4242,
                     '{\"radiant_team_ids\":[1003],\"dire_team_ids\":[999],\"bet_lock_until\":20000,\"is_draft\":false}');
             INSERT INTO matches (
                 match_id, guild_id, team1_players, team2_players, winning_team
             ) VALUES (701, 4242, '[1003]', '[999]', 1);",
        )
        .expect("ledger and match fixture");
    drop(connection);

    let betting = BettingServiceRepository::new(fixture.path());
    betting
        .place_bet_atomic(PlaceBetRequest {
            guild_id: Some(guild_id),
            pending_match_id: 700,
            discord_id,
            team: BettingTeam::Radiant,
            amount: 100,
            bet_time: 10_000,
            leverage: 1,
            max_debt: 500,
            is_blind: false,
            odds_at_placement: None,
        })
        .expect("place taxable bet");
    let settlement = betting
        .settle_pending_bets_with_adjustments_atomic(
            701,
            Some(guild_id),
            9_000,
            Some(700),
            BettingTeam::Radiant,
            BettingMode::House,
            &BetSettlementAdjustments {
                bankruptcy_keep_basis_points: Some(7_500),
                vanity_tax_basis_points: 1_000,
                vanity_taxable_ids: BTreeSet::from([discord_id]),
                ..BetSettlementAdjustments::default()
            },
        )
        .expect("settle taxable bet");
    assert!(settlement.bankruptcy_penalties.is_empty());
    assert_eq!(settlement.vanity_taxes[&discord_id], 10);
    assert_eq!(settlement.winners[0].payout, 200);
    assert_eq!(settlement.winners[0].balance_after, 593);

    let repository = fixture.stats_repository();
    let history = repository
        .player_bet_history(discord_id, Some(guild_id))
        .expect("taxed history");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].vanity_tax, 10);
    assert_eq!(history[0].profit, 90);
    let metrics = repository
        .bulk_gambling_metrics(Some(guild_id), Some(&[discord_id]))
        .expect("taxed metrics");
    assert_eq!(metrics[&discord_id].net_pnl, 90);
    let leaderboard = fixture
        .service()
        .get_leaderboard(Some(guild_id), 10, 1)
        .expect("taxed guild summary");
    assert_eq!(leaderboard.top_earners[0].discord_id, discord_id);
    assert_eq!(leaderboard.top_earners[0].net_pnl, 90);

    let vanity_ledger_delta = Connection::open(fixture.path())
        .expect("open ledger")
        .query_row(
            "SELECT delta FROM economy_ledger_entries
             WHERE guild_id = ?1 AND account_id = ?2 AND source = 'vanity_tax'",
            params![guild_id, discord_id],
            |row| row.get::<_, i64>(0),
        )
        .expect("vanity ledger row");
    assert_eq!(vanity_ledger_delta, -10);
}

#[test]
fn test_bulk_current_streaks_and_degen_scores_match_point_paths() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 500, DEFAULT_GUILD);
    fixture.add_player(1002, 500, DEFAULT_GUILD);
    for winner in [
        BettingTeam::Radiant,
        BettingTeam::Dire,
        BettingTeam::Radiant,
        BettingTeam::Radiant,
    ] {
        fixture.settle_bet(1001, 10, 1, winner, DEFAULT_GUILD);
    }
    for winner in [
        BettingTeam::Radiant,
        BettingTeam::Dire,
        BettingTeam::Dire,
        BettingTeam::Dire,
    ] {
        fixture.settle_bet(1002, 10, 1, winner, DEFAULT_GUILD);
    }
    let repository = fixture.stats_repository();
    assert_eq!(
        repository
            .current_bet_streaks_bulk(&[1001, 1002, 9999], None)
            .expect("bulk streaks"),
        BTreeMap::from([(1001, 2), (1002, -3)])
    );
    let service = fixture.service();
    let bulk = service
        .calculate_degen_scores_bulk(&[1001, 1002], None)
        .expect("bulk scores");
    assert_eq!(
        bulk[&1001],
        service
            .calculate_degen_score(1001, None)
            .expect("point score")
    );
    assert_eq!(
        bulk[&1002],
        service
            .calculate_degen_score(1002, None)
            .expect("point score")
    );
}

#[test]
fn test_bulk_degen_scores_use_consolidated_gambling_metrics() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 500, DEFAULT_GUILD);
    fixture.settle_bet(1001, 10, 5, BettingTeam::Radiant, DEFAULT_GUILD);
    let expected = fixture
        .service()
        .calculate_degen_score(1001, None)
        .expect("point score");
    let service = GamblingStatsService::new(CountingPort::new(fixture.stats_repository()));
    let scores = service
        .calculate_degen_scores_bulk(&[1001, 1001, 9999], None)
        .expect("bulk scores");
    assert_eq!(
        service.port().metric_calls.borrow().as_slice(),
        &[(None, Some(vec![1001, 9999]))]
    );
    assert_eq!(scores[&1001], expected);
    assert_eq!(scores[&9999].total, 0);
}

#[test]
fn test_gambling_leaderboard_uses_consolidated_metrics() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 500, DEFAULT_GUILD);
    for _ in 0..3 {
        fixture.settle_bet(1001, 10, 1, BettingTeam::Radiant, DEFAULT_GUILD);
    }
    let service = GamblingStatsService::new(CountingPort::new(fixture.stats_repository()));
    let leaderboard = service.get_leaderboard(None, 5, 3).expect("leaderboard");
    assert_eq!(
        service.port().metric_calls.borrow().as_slice(),
        &[(None, None)]
    );
    assert_eq!(
        leaderboard
            .top_earners
            .iter()
            .map(|entry| entry.discord_id)
            .collect::<Vec<_>>(),
        [1001]
    );
    assert_eq!(leaderboard.server_stats.total_bets, 3);
}

#[test]
fn test_get_player_stats_no_bets() {
    let fixture = Fixture::new();
    fixture.add_player(1001, 100, DEFAULT_GUILD);
    assert!(
        fixture
            .service()
            .get_player_stats(1001, None)
            .expect("stats")
            .is_none()
    );
}

#[test]
fn test_get_player_stats_with_bets() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 200, DEFAULT_GUILD);
    for winner in [
        BettingTeam::Radiant,
        BettingTeam::Radiant,
        BettingTeam::Radiant,
        BettingTeam::Dire,
        BettingTeam::Dire,
    ] {
        fixture.settle_bet(1001, 10, 1, winner, DEFAULT_GUILD);
    }
    let stats = fixture
        .service()
        .get_player_stats(1001, None)
        .expect("stats")
        .expect("player stats");
    assert_eq!((stats.total_bets, stats.wins, stats.losses), (5, 3, 2));
    close(stats.win_rate, 3.0 / 5.0);
    assert_eq!(stats.net_pnl, 10);
    assert_eq!(stats.total_wagered, 50);
    assert_eq!(
        (stats.best_streak, stats.worst_streak, stats.current_streak),
        (3, -2, -2)
    );
}

#[test]
fn test_get_player_stats_derives_leverage_from_history() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 500, DEFAULT_GUILD);
    for leverage in [1, 2, 5] {
        fixture.settle_bet(1001, 10, leverage, BettingTeam::Radiant, DEFAULT_GUILD);
    }
    Connection::open(fixture.path())
        .expect("open fixture")
        .execute(
            "UPDATE bets SET leverage = NULL WHERE discord_id = 1001 AND leverage = 1",
            [],
        )
        .expect("legacy null leverage");
    let service = GamblingStatsService::new(CountingPort::new(fixture.stats_repository()));
    let stats = service
        .get_player_stats(1001, None)
        .expect("stats")
        .expect("player stats");
    assert_eq!(
        stats.leverage_distribution,
        BTreeMap::from([(1, 1), (2, 1), (5, 1)])
    );
    assert_eq!(service.port().history_calls.get(), 1);
    assert_eq!(
        stats.degen_score,
        service
            .calculate_degen_score_with_data(
                1001,
                None,
                Some(
                    fixture
                        .stats_repository()
                        .player_bet_history(1001, None)
                        .expect("history"),
                ),
                Some(BTreeMap::from([(1, 1), (2, 1), (5, 1)])),
            )
            .expect("prefetched score")
    );
}

#[test]
fn test_calculate_degen_score_derives_leverage_without_repository_query() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 200, DEFAULT_GUILD);
    fixture.settle_bet(1001, 10, 5, BettingTeam::Radiant, DEFAULT_GUILD);
    let service = GamblingStatsService::new(CountingPort::new(fixture.stats_repository()));
    let degen = service
        .calculate_degen_score(1001, None)
        .expect("degen score");
    assert_eq!(degen.max_leverage_score, 25);
    assert_eq!(service.port().history_calls.get(), 1);
}

#[test]
fn test_calculate_degen_score_reuses_prefetched_empty_bet_data() {
    let fixture = Fixture::new();
    let service = GamblingStatsService::new(CountingPort::new(fixture.stats_repository()));
    let degen = service
        .calculate_degen_score_with_data(1001, None, Some(Vec::new()), Some(BTreeMap::new()))
        .expect("empty prefetched score");
    assert_eq!(degen.total, 0);
    assert_eq!(service.port().history_calls.get(), 0);
}

#[test]
fn test_point_degen_score_includes_negative_loan_bonus() {
    let fixture = Fixture::new();
    fixture.add_player(1001, 100, DEFAULT_GUILD);
    Connection::open(fixture.path())
        .expect("open fixture")
        .execute(
            "INSERT INTO loan_state (
                 discord_id, guild_id, total_loans_taken, negative_loans_taken
             ) VALUES (1001, 0, 2, 2)",
            [],
        )
        .expect("negative loan state");

    let degen = fixture
        .service()
        .calculate_degen_score_with_data(1001, None, Some(Vec::new()), Some(BTreeMap::new()))
        .expect("negative-loan degen score");

    assert_eq!(degen.negative_loan_bonus, 50);
    assert_eq!(degen.total, 50);
    assert_eq!(
        degen.flavor_texts.first().map(String::as_str),
        Some("borrowed while broke x2")
    );
}

#[test]
fn test_high_leverage_increases_degen_score() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 200, DEFAULT_GUILD);
    fixture.settle_bet(1001, 10, 1, BettingTeam::Radiant, DEFAULT_GUILD);
    fixture.settle_bet(1001, 10, 1, BettingTeam::Dire, DEFAULT_GUILD);
    fixture.add_player(1002, 200, DEFAULT_GUILD);
    fixture.settle_bet(1002, 10, 5, BettingTeam::Radiant, DEFAULT_GUILD);
    fixture.settle_bet(1002, 10, 5, BettingTeam::Radiant, DEFAULT_GUILD);
    let service = fixture.service();
    let low = service
        .calculate_degen_score(1001, None)
        .expect("low leverage");
    let high = service
        .calculate_degen_score(1002, None)
        .expect("high leverage");
    for degen in [&low, &high] {
        assert!((0..=100).contains(&degen.total));
        assert!(
            [
                "Casual",
                "Recreational",
                "Committed",
                "Degenerate",
                "Menace",
                "Legendary Degen"
            ]
            .contains(&degen.title)
        );
    }
    assert!(high.max_leverage_score > low.max_leverage_score);
    assert!(high.total > low.total);
}

#[test]
fn test_leaderboard_empty() {
    let fixture = Fixture::new();
    let leaderboard = fixture
        .service()
        .get_leaderboard(None, 5, 3)
        .expect("empty leaderboard");
    assert!(leaderboard.top_earners.is_empty());
    assert!(leaderboard.down_bad.is_empty());
    assert!(leaderboard.hall_of_degen.is_empty());
    assert_eq!(leaderboard.total_loans, 0);
}

#[test]
fn test_server_stats_include_bettors_below_minimum() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 200, DEFAULT_GUILD);
    for _ in 0..3 {
        fixture.settle_bet(1001, 10, 1, BettingTeam::Radiant, DEFAULT_GUILD);
    }
    fixture.add_player(1002, 200, DEFAULT_GUILD);
    for _ in 0..2 {
        fixture.settle_bet(1002, 20, 1, BettingTeam::Dire, DEFAULT_GUILD);
    }
    Connection::open(fixture.path())
        .expect("open fixture")
        .execute(
            "INSERT INTO bankruptcy_state (
                 discord_id, guild_id, bankruptcy_count
             ) VALUES (1002, 0, 1)",
            [],
        )
        .expect("bankruptcy state");
    let leaderboard = fixture
        .service()
        .get_leaderboard(None, 5, 3)
        .expect("leaderboard");
    assert_eq!(
        leaderboard
            .top_earners
            .iter()
            .map(|entry| entry.discord_id)
            .collect::<Vec<_>>(),
        [1001]
    );
    for section in [
        &leaderboard.top_earners,
        &leaderboard.down_bad,
        &leaderboard.hall_of_degen,
        &leaderboard.biggest_gamblers,
    ] {
        assert!(section.iter().all(|entry| entry.discord_id != 1002));
    }
    assert_eq!(
        leaderboard.server_stats,
        ServerStats {
            total_bets: 5,
            total_wagered: 70,
            unique_gamblers: 2,
            avg_bet_size: 14,
            total_bankruptcies: 1,
        }
    );
}

#[test]
fn test_leaderboard_with_only_ineligible_bettors_keeps_server_stats() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 100, DEFAULT_GUILD);
    fixture.settle_bet(1001, 25, 1, BettingTeam::Radiant, DEFAULT_GUILD);
    let leaderboard = fixture
        .service()
        .get_leaderboard(None, 5, 3)
        .expect("leaderboard");
    assert!(leaderboard.top_earners.is_empty());
    assert!(leaderboard.down_bad.is_empty());
    assert!(leaderboard.hall_of_degen.is_empty());
    assert!(leaderboard.biggest_gamblers.is_empty());
    assert_eq!(leaderboard.server_stats.total_bets, 1);
    assert_eq!(leaderboard.server_stats.total_wagered, 25);
    assert_eq!(leaderboard.server_stats.unique_gamblers, 1);
}

#[test]
fn test_leaderboard_section_ties_use_discord_id() {
    let mut fixture = Fixture::new();
    for (discord_id, winner) in [
        (1002, BettingTeam::Radiant),
        (2002, BettingTeam::Dire),
        (1001, BettingTeam::Radiant),
        (2001, BettingTeam::Dire),
    ] {
        fixture.add_player(discord_id, 200, DEFAULT_GUILD);
        for _ in 0..3 {
            fixture.settle_bet(discord_id, 10, 1, winner, DEFAULT_GUILD);
        }
    }
    let leaderboard = fixture
        .service()
        .get_leaderboard(None, 10, 3)
        .expect("leaderboard");
    assert_eq!(ids(&leaderboard.top_earners), [1001, 1002]);
    assert_eq!(ids(&leaderboard.down_bad), [2001, 2002]);
    assert_eq!(ids(&leaderboard.hall_of_degen), [1001, 1002, 2001, 2002]);
    assert_eq!(ids(&leaderboard.biggest_gamblers), [1001, 1002, 2001, 2002]);
}

#[test]
fn test_top_earners_excludes_negative_pnl() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 200, DEFAULT_GUILD);
    for _ in 0..3 {
        fixture.settle_bet(1001, 10, 1, BettingTeam::Dire, DEFAULT_GUILD);
    }
    let leaderboard = fixture
        .service()
        .get_leaderboard(None, 5, 3)
        .expect("leaderboard");
    assert!(leaderboard.top_earners.is_empty());
    assert_eq!(ids(&leaderboard.down_bad), [1001]);
}

#[test]
fn test_leaderboard_total_loans() {
    let mut fixture = Fixture::new();
    for (discord_id, loans, bets, winner) in [
        (1001, 5, 3, BettingTeam::Radiant),
        (1002, 3, 3, BettingTeam::Dire),
        (1003, 10, 2, BettingTeam::Radiant),
    ] {
        fixture.add_player(discord_id, 100, DEFAULT_GUILD);
        Connection::open(fixture.path())
            .expect("open fixture")
            .execute(
                "INSERT INTO loan_state (
                     discord_id, guild_id, total_loans_taken, negative_loans_taken
                 ) VALUES (?1, 0, ?2, 0)",
                params![discord_id, loans],
            )
            .expect("loan state");
        for _ in 0..bets {
            fixture.settle_bet(discord_id, 10, 1, winner, DEFAULT_GUILD);
        }
    }
    assert_eq!(
        fixture
            .service()
            .get_leaderboard(None, 5, 3)
            .expect("leaderboard")
            .total_loans,
        18
    );
}

#[test]
fn test_cumulative_pnl_series() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 200, DEFAULT_GUILD);
    fixture.settle_bet(1001, 10, 1, BettingTeam::Radiant, DEFAULT_GUILD);
    fixture.settle_bet(1001, 10, 1, BettingTeam::Dire, DEFAULT_GUILD);
    fixture.settle_bet(1001, 10, 1, BettingTeam::Radiant, DEFAULT_GUILD);
    let series = fixture
        .service()
        .cumulative_pnl_series(1001, None)
        .expect("P&L series");
    assert_eq!(series.len(), 3);
    assert_eq!((series[0].event_number, series[0].cumulative_pnl), (1, 10));
    assert_eq!(
        series[0].event,
        PnlEvent {
            amount: 10,
            leverage: 1,
            effective_bet: 10,
            outcome: GamblingOutcome::Won,
            profit: 10,
            team: Some(BetSide::Radiant),
            source: GamblingSource::Bet,
        }
    );
    assert_eq!(series[1].cumulative_pnl, 0);
    assert_eq!(series[2].cumulative_pnl, 10);
}

#[test]
fn test_cumulative_pnl_series_with_double_or_nothing() {
    let fixture = Fixture::new();
    fixture.add_player(1001, 1_000, DEFAULT_GUILD);
    let repository = fixture.stats_repository();
    repository
        .log_double_or_nothing_atomic(DoubleOrNothingWrite {
            discord_id: 1001,
            guild_id: None,
            cost: 50,
            balance_before: 950,
            balance_after: 1_900,
            won: true,
            spin_time: 1_000,
        })
        .expect("winning spin");
    repository
        .log_double_or_nothing_atomic(DoubleOrNothingWrite {
            discord_id: 1001,
            guild_id: None,
            cost: 50,
            balance_before: 1_850,
            balance_after: 0,
            won: false,
            spin_time: 1_100,
        })
        .expect("losing spin");
    let series = fixture
        .service()
        .cumulative_pnl_series(1001, None)
        .expect("P&L series");
    assert_eq!(series.len(), 2);
    assert_eq!((series[0].event_number, series[0].cumulative_pnl), (1, 900));
    assert_eq!(series[0].event.source, GamblingSource::DoubleOrNothing);
    assert_eq!(series[0].event.outcome, GamblingOutcome::Won);
    assert_eq!(series[0].event.profit, 900);
    assert_eq!(series[0].event.amount, 1_000);
    assert_eq!(series[0].event.effective_bet, 950);
    assert_eq!(
        (series[1].event_number, series[1].cumulative_pnl),
        (2, -1_000)
    );
    assert_eq!(series[1].event.outcome, GamblingOutcome::Lost);
    assert_eq!(series[1].event.profit, -1_900);
    assert_eq!(series[1].event.amount, 1_900);
    assert_eq!(series[1].event.effective_bet, 1_850);
    let cooldown = Connection::open(fixture.path())
        .expect("open fixture")
        .query_row(
            "SELECT last_double_or_nothing FROM players
             WHERE discord_id = 1001 AND guild_id = 0",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("cooldown");
    assert_eq!(cooldown, 1_100);
}

#[test]
fn test_paper_hands_detection() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 100, DEFAULT_GUILD);
    fixture.record_match_without_bet(1001, DEFAULT_GUILD);
    assert_eq!(
        fixture
            .stats_repository()
            .paper_hands(1001, None)
            .expect("paper hands"),
        PaperHands {
            matches_played: 1,
            matches_bet_on_self: 0,
            paper_hands_count: 1,
        }
    );
}

#[test]
fn test_payout_stored_on_settlement() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 100, DEFAULT_GUILD);
    fixture.settle_bet(1001, 10, 1, BettingTeam::Radiant, DEFAULT_GUILD);
    let history = fixture
        .stats_repository()
        .player_bet_history(1001, None)
        .expect("history");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].payout, Some(20));
}

#[test]
fn test_payout_null_for_losers() {
    let mut fixture = Fixture::new();
    fixture.add_player(1001, 100, DEFAULT_GUILD);
    fixture.settle_bet(1001, 10, 1, BettingTeam::Dire, DEFAULT_GUILD);
    let payout = Connection::open(fixture.path())
        .expect("open fixture")
        .query_row(
            "SELECT payout FROM bets WHERE discord_id = 1001",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )
        .expect("raw payout");
    assert_eq!(payout, None);
}

fn ids(entries: &[LeaderboardEntry]) -> Vec<i64> {
    entries.iter().map(|entry| entry.discord_id).collect()
}

#[test]
fn test_auto_bet_performance_groups_generic_and_each_target() {
    let history = [
        auto_history_bet(1, BetSide::Radiant, true, 10, 8, None, None),
        auto_history_bet(2, BetSide::Dire, true, 20, 15, Some(9_001), Some("long")),
        auto_history_bet(3, BetSide::Radiant, true, 5, -5, Some(9_001), Some("short")),
        auto_history_bet(4, BetSide::Dire, true, 12, -12, Some(9_002), Some("short")),
    ];
    let performance = calculate_auto_bet_performance(&history);

    assert_eq!(performance.total.bet_count, 4);
    assert_eq!(performance.total.total_wagered, 47);
    assert_eq!(performance.total.net_pnl, 6);
    assert_eq!(performance.generic.bet_count, 1);
    assert_eq!(performance.generic.net_pnl, 8);
    let targets = performance
        .targets
        .iter()
        .map(|target| (target.target_id, target))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(targets[&9_001].directions, ["long", "short"]);
    assert_eq!(targets[&9_001].bet_count, 2);
    assert_eq!(targets[&9_001].wins, 1);
    assert_eq!(targets[&9_001].total_wagered, 25);
    assert_eq!(targets[&9_001].net_pnl, 10);
    assert_eq!(targets[&9_002].directions, ["short"]);
    assert_eq!(targets[&9_002].net_pnl, -12);
}

#[test]
fn test_opposite_manual_arbitrage_is_counted_once_with_multiple_opposing_autos() {
    let history = [
        auto_history_bet(
            10,
            BetSide::Radiant,
            true,
            10,
            -10,
            Some(9_001),
            Some("long"),
        ),
        auto_history_bet(
            10,
            BetSide::Radiant,
            true,
            15,
            -15,
            Some(9_002),
            Some("short"),
        ),
        auto_history_bet(10, BetSide::Dire, false, 30, 18, None, None),
    ];
    let arbitrage = calculate_auto_bet_performance(&history).arbitrage;

    assert_eq!(arbitrage.bet_count, 1);
    assert_eq!(arbitrage.wins, 1);
    assert_eq!(arbitrage.total_wagered, 30);
    assert_eq!(arbitrage.net_pnl, 18);
}

#[test]
fn test_same_side_manual_bet_and_unpaired_opposite_bet_are_not_arbitrage() {
    let history = [
        auto_history_bet(20, BetSide::Radiant, true, 10, 7, None, None),
        auto_history_bet(20, BetSide::Radiant, false, 5, 4, None, None),
        auto_history_bet(21, BetSide::Dire, false, 8, -8, None, None),
    ];
    let arbitrage = calculate_auto_bet_performance(&history).arbitrage;

    assert_eq!(arbitrage, AutoBetGroupStats::default());
}
