use std::collections::BTreeSet;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::schema_manager::initialize_or_migrate;

const GUILD: i64 = 9_001;

struct Fixture {
    file: NamedTempFile,
    repository: MatchRecordingRepository,
}

impl Fixture {
    fn new() -> Self {
        let file = NamedTempFile::new().expect("temporary SQLite file");
        let connection = Connection::open(file.path()).expect("open fixture database");
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = OFF;
                 CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_username TEXT NOT NULL,
                     wins INTEGER DEFAULT 0,
                     losses INTEGER DEFAULT 0,
                     glicko_rating REAL,
                     glicko_rd REAL,
                     glicko_volatility REAL,
                     exclusion_count INTEGER DEFAULT 0,
                     jopacoin_balance INTEGER DEFAULT 3,
                     lowest_balance_ever INTEGER,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE matches (
                     match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     team1_players TEXT NOT NULL,
                     team2_players TEXT NOT NULL,
                     winning_team INTEGER,
                     lobby_type TEXT,
                     balancing_rating_system TEXT,
                     betting_mode TEXT,
                     win_reward_jc INTEGER,
                     pending_match_id INTEGER,
                     valve_match_id INTEGER,
                     duration_seconds INTEGER,
                     radiant_score INTEGER,
                     dire_score INTEGER,
                     game_mode INTEGER,
                     enrichment_data TEXT,
                     enrichment_source TEXT,
                     enrichment_confidence REAL,
                     match_date TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                 );
                 CREATE UNIQUE INDEX idx_matches_guild_pending
                     ON matches(guild_id, pending_match_id)
                     WHERE pending_match_id IS NOT NULL;
                 CREATE TABLE match_participants (
                     match_id INTEGER NOT NULL,
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     team_number INTEGER,
                     won INTEGER,
                     side TEXT,
                     hero_id INTEGER,
                     kills INTEGER,
                     deaths INTEGER,
                     assists INTEGER,
                     gpm INTEGER,
                     xpm INTEGER,
                     hero_damage INTEGER,
                     tower_damage INTEGER,
                     last_hits INTEGER,
                     denies INTEGER,
                     net_worth INTEGER,
                     hero_healing INTEGER,
                     lane_role INTEGER,
                     lane_efficiency INTEGER,
                     gold_at_10 INTEGER,
                     derived_role TEXT,
                     towers_killed INTEGER,
                     roshans_killed INTEGER,
                     teamfight_participation REAL,
                     obs_placed INTEGER,
                     sen_placed INTEGER,
                     camps_stacked INTEGER,
                     rune_pickups INTEGER,
                     firstblood_claimed INTEGER,
                     stuns REAL,
                     fantasy_points REAL,
                     PRIMARY KEY (match_id, discord_id)
                 );
                 CREATE TABLE wrapped_enrichment_facts (
                     guild_id INTEGER NOT NULL,
                     match_id INTEGER NOT NULL,
                     discord_id INTEGER NOT NULL,
                     actions_per_min INTEGER,
                     courier_kills INTEGER,
                     pings INTEGER,
                     rapier_count INTEGER NOT NULL DEFAULT 0,
                     lane_role INTEGER,
                     comeback INTEGER,
                     throw INTEGER,
                     PRIMARY KEY (guild_id, match_id, discord_id)
                 );
                 CREATE TABLE match_bans (
                     match_id INTEGER NOT NULL,
                     ban_index INTEGER NOT NULL,
                     team INTEGER NOT NULL,
                     hero_id INTEGER NOT NULL,
                     PRIMARY KEY (match_id, ban_index)
                 );
                 CREATE TABLE openskill_replay_jobs (
                     guild_id INTEGER PRIMARY KEY,
                     reason TEXT NOT NULL,
                     requested_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                     last_error TEXT
                 );
                 CREATE TABLE rating_history (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     discord_id INTEGER,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     rating REAL,
                     rating_before REAL,
                     rd_before REAL,
                     rd_after REAL,
                     volatility_before REAL,
                     volatility_after REAL,
                     team_number INTEGER,
                     won INTEGER,
                     match_id INTEGER
                 );
                 CREATE TABLE bets (
                     bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     match_id INTEGER,
                     discord_id INTEGER NOT NULL,
                     team_bet_on TEXT NOT NULL,
                     amount INTEGER NOT NULL,
                     bet_time INTEGER NOT NULL,
                     created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                 );
                 CREATE TABLE loan_state (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     last_loan_at INTEGER,
                     total_loans_taken INTEGER DEFAULT 0,
                     total_fees_paid INTEGER DEFAULT 0,
                     negative_loans_taken INTEGER DEFAULT 0,
                     outstanding_principal INTEGER DEFAULT 0,
                     outstanding_fee INTEGER DEFAULT 0,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE nonprofit_fund (
                     guild_id INTEGER PRIMARY KEY DEFAULT 0,
                     total_collected INTEGER DEFAULT 0,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                 );
                 CREATE TABLE player_pairings (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL,
                     player1_id INTEGER NOT NULL,
                     player2_id INTEGER NOT NULL,
                     games_together INTEGER NOT NULL DEFAULT 0,
                     wins_together INTEGER NOT NULL DEFAULT 0,
                     games_against INTEGER NOT NULL DEFAULT 0,
                     player1_wins_against INTEGER NOT NULL DEFAULT 0,
                     last_match_id INTEGER,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     UNIQUE(guild_id, player1_id, player2_id)
                 );",
            )
            .expect("create disposable match-recording schema");
        drop(connection);
        let repository = MatchRecordingRepository::new(file.path());
        Self { file, repository }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.file.path()).expect("open fixture connection")
    }

    fn seed(&self, ids: &[i64]) {
        for discord_id in ids {
            self.repository
                .add_player(
                    *discord_id,
                    Some(GUILD),
                    &format!("Player{discord_id}"),
                    1_500.0,
                )
                .expect("seed player");
        }
    }

    fn player(&self, discord_id: i64) -> PlayerRecord {
        self.repository
            .get_player(discord_id, Some(GUILD))
            .expect("read player")
            .expect("seeded player")
    }
}

fn teams(start: i64) -> (Vec<i64>, Vec<i64>) {
    (
        (start..start + 5).collect(),
        (start + 5..start + 10).collect(),
    )
}

fn standard<'a>(radiant: &'a [i64], dire: &'a [i64], winner: TeamSide) -> MatchRecordRequest<'a> {
    MatchRecordRequest::standard(Some(GUILD), radiant, dire, winner)
}

fn recorded_enrichment_match(fixture: &Fixture, start: i64) -> i64 {
    let (radiant, dire) = teams(start);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record enrichment fixture")
        .match_id
}

fn apply_enrichment_payload(
    fixture: &Fixture,
    match_id: i64,
    enrichment_data: &str,
    source: &str,
) -> usize {
    fixture
        .repository
        .apply_enrichment_atomic(MatchEnrichmentRequest {
            match_id,
            guild_id: Some(GUILD),
            valve_match_id: 8_181_518_332,
            duration_seconds: 2_400,
            radiant_score: 35,
            dire_score: 22,
            game_mode: 2,
            enrichment_data: Some(enrichment_data),
            enrichment_source: Some(source),
            enrichment_confidence: None,
            participant_updates: &[],
            wrapped_facts: &[],
        })
        .expect("apply enrichment payload")
}

fn projected_bans(fixture: &Fixture, match_id: i64) -> Vec<(i64, i64, i64)> {
    fixture
        .connection()
        .prepare(
            "SELECT ban_index, team, hero_id FROM match_bans
             WHERE match_id = ?1 ORDER BY ban_index",
        )
        .and_then(|mut statement| {
            statement
                .query_map([match_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .expect("read normalized bans")
}

type WrappedProjectionRow = (
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

fn projected_wrapped_facts(fixture: &Fixture, match_id: i64) -> Vec<WrappedProjectionRow> {
    fixture
        .connection()
        .prepare(
            "SELECT discord_id,actions_per_min,courier_kills,pings,
                    rapier_count,lane_role,comeback,throw
               FROM wrapped_enrichment_facts
              WHERE guild_id=?1 AND match_id=?2 ORDER BY discord_id",
        )
        .and_then(|mut statement| {
            statement
                .query_map(params![GUILD, match_id], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .expect("read compact Wrapped projection")
}

fn fantasy_update(discord_id: i64, fantasy_points: f64) -> EnrichmentParticipantUpdate {
    EnrichmentParticipantUpdate {
        discord_id,
        hero_id: None,
        kills: None,
        deaths: None,
        assists: None,
        gpm: None,
        xpm: None,
        hero_damage: None,
        tower_damage: None,
        last_hits: None,
        denies: None,
        net_worth: None,
        hero_healing: None,
        lane_role: None,
        lane_efficiency: None,
        gold_at_10: None,
        towers_killed: None,
        roshans_killed: None,
        teamfight_participation: None,
        obs_placed: None,
        sen_placed: None,
        camps_stacked: None,
        rune_pickups: None,
        firstblood_claimed: None,
        stuns: None,
        fantasy_points: Some(fantasy_points),
    }
}

#[test]
fn test_multiple_matches_accumulate_correctly() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(11_001);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    for winner in [TeamSide::Radiant, TeamSide::Radiant, TeamSide::Dire] {
        fixture
            .repository
            .record_match_atomic(standard(&radiant, &dire, winner))
            .expect("record match");
    }
    for discord_id in radiant {
        let player = fixture.player(discord_id);
        assert_eq!((player.wins, player.losses), (2, 1));
    }
    for discord_id in dire {
        let player = fixture.player(discord_id);
        assert_eq!((player.wins, player.losses), (1, 2));
    }
}

#[test]
fn test_existing_wins_losses_are_incremented() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(12_001);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    let connection = fixture.connection();
    for discord_id in &radiant {
        connection
            .execute(
                "UPDATE players SET wins = 2, losses = 3 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, GUILD],
            )
            .expect("seed radiant stats");
    }
    for discord_id in &dire {
        connection
            .execute(
                "UPDATE players SET wins = 1, losses = 4 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, GUILD],
            )
            .expect("seed dire stats");
    }
    drop(connection);
    fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Dire))
        .expect("record match");
    for discord_id in radiant {
        let player = fixture.player(discord_id);
        assert_eq!((player.wins, player.losses), (2, 4));
    }
    for discord_id in dire {
        let player = fixture.player(discord_id);
        assert_eq!((player.wins, player.losses), (2, 4));
    }
}

#[test]
fn test_participants_table_has_correct_side_and_won_flags() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(13_001);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    let result = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record match");
    let participants = fixture
        .repository
        .get_participants(result.match_id, Some(GUILD))
        .expect("participants");
    assert_eq!(participants.len(), 10);
    for participant in participants {
        if radiant.contains(&participant.discord_id) {
            assert_eq!(
                (
                    participant.team_number,
                    participant.side.as_str(),
                    participant.won
                ),
                (1, "radiant", true)
            );
        } else {
            assert_eq!(
                (
                    participant.team_number,
                    participant.side.as_str(),
                    participant.won
                ),
                (2, "dire", false)
            );
        }
    }
}

#[test]
fn test_radiant_dire_team_mapping() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(3_001);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    for winner in [TeamSide::Radiant, TeamSide::Dire] {
        let result = fixture
            .repository
            .record_match_atomic(standard(&radiant, &dire, winner))
            .expect("record mapping match");
        let participants = fixture
            .repository
            .get_participants(result.match_id, Some(GUILD))
            .expect("participants");
        assert!(
            participants.iter().all(|row| {
                row.won == ((row.side == "radiant") == (winner == TeamSide::Radiant))
            })
        );
    }
}

#[test]
fn test_exact_bug_scenario_dire_wins() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(90_001);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    let result = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Dire))
        .expect("record Dire win");
    assert_eq!(result.winning_player_ids, dire);
    assert_eq!(result.losing_player_ids, radiant);
    let stored = fixture
        .repository
        .get_match(result.match_id, Some(GUILD))
        .expect("read match")
        .expect("stored match");
    assert_eq!(stored.winning_team, 2);
    for discord_id in &stored.dire_ids {
        assert_eq!(
            (
                fixture.player(*discord_id).wins,
                fixture.player(*discord_id).losses
            ),
            (1, 0)
        );
    }
    for discord_id in &stored.radiant_ids {
        assert_eq!(
            (
                fixture.player(*discord_id).wins,
                fixture.player(*discord_id).losses
            ),
            (0, 1)
        );
    }
}

#[test]
fn test_exact_bug_scenario_radiant_wins() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(91_001);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    let result = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record Radiant win");
    let stored = fixture
        .repository
        .get_match(result.match_id, Some(GUILD))
        .expect("read match")
        .expect("stored match");
    assert_eq!(
        (stored.radiant_ids, stored.dire_ids, stored.winning_team),
        (radiant.clone(), dire.clone(), 1)
    );
    assert!(
        radiant
            .iter()
            .all(|discord_id| fixture.player(*discord_id).wins == 1)
    );
    assert!(
        dire.iter()
            .all(|discord_id| fixture.player(*discord_id).losses == 1)
    );
}

#[test]
fn test_glicko_ratings_persisted_after_record_match() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(7_001);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    let result = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record rated match");
    assert!(
        radiant
            .iter()
            .all(|discord_id| fixture.player(*discord_id).glicko_rating > 1_500.0)
    );
    assert!(
        dire.iter()
            .all(|discord_id| fixture.player(*discord_id).glicko_rating < 1_500.0)
    );
    let history = fixture
        .repository
        .rating_history(result.match_id, Some(GUILD))
        .expect("rating history");
    assert_eq!(history.len(), 10);
    assert!(history.iter().all(|row| row.rating_before == 1_500.0));
    assert!(
        history
            .iter()
            .all(|row| (row.rating > row.rating_before) == row.won)
    );
}

#[test]
fn test_bets_settle_with_house() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(1_001);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    let participant = radiant[0];
    let spectator = 9_000;
    fixture.seed(&[spectator]);
    fixture
        .repository
        .add_balance(participant, Some(GUILD), 20)
        .expect("top up participant");
    fixture
        .repository
        .add_balance(spectator, Some(GUILD), 10)
        .expect("top up spectator");
    fixture
        .repository
        .place_bet_atomic(Some(GUILD), participant, TeamSide::Radiant, 5, 100)
        .expect("winning bet");
    fixture
        .repository
        .place_bet_atomic(Some(GUILD), spectator, TeamSide::Dire, 5, 100)
        .expect("losing bet");
    let mut request = standard(&radiant, &dire, TeamSide::Radiant);
    request.pending_bet_since = 100;
    request.update_ratings = false;
    let result = fixture
        .repository
        .record_match_atomic(request)
        .expect("record match");
    assert_eq!(result.bet_distributions.winners[0].discord_id, participant);
    assert_eq!(result.bet_distributions.losers[0].discord_id, spectator);
    assert_eq!(
        result.jc_changes[&participant],
        JcChange { payout: 10, bet: 5 }
    );
    assert_eq!(
        result.jc_changes[&spectator],
        JcChange { payout: 0, bet: -5 }
    );
    assert_eq!(fixture.player(participant).balance, 38);
    assert_eq!(fixture.player(spectator).balance, 8);
}

#[test]
fn test_lobby_rewards_pay_ten_to_winners_and_five_to_others() {
    let fixture = Fixture::new();
    let all = (5_101..5_113).collect::<Vec<_>>();
    let radiant = all[..5].to_vec();
    let dire = all[5..10].to_vec();
    let excluded = all[10..].to_vec();
    fixture.seed(&all);
    for discord_id in &all {
        fixture
            .repository
            .set_balance(*discord_id, Some(GUILD), 0)
            .expect("zero balance");
    }
    let mut request = standard(&radiant, &dire, TeamSide::Radiant);
    request.rewarded_excluded_ids = &excluded;
    request.all_excluded_ids = &excluded;
    let result = fixture
        .repository
        .record_match_atomic(request)
        .expect("record match");
    for discord_id in &radiant {
        assert_eq!(
            (
                fixture.player(*discord_id).balance,
                result.jc_changes[discord_id].payout
            ),
            (10, 10)
        );
    }
    for discord_id in dire.iter().chain(excluded.iter()) {
        assert_eq!(
            (
                fixture.player(*discord_id).balance,
                result.jc_changes[discord_id].payout
            ),
            (5, 5)
        );
    }
}

#[test]
fn test_readycheck_nonresponders_receive_no_match_jc() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(5_151);
    let nonresponder = 5_161;
    fixture.seed(&[radiant.clone(), dire.clone(), vec![nonresponder]].concat());
    fixture
        .repository
        .set_balance(nonresponder, Some(GUILD), 0)
        .expect("zero nonresponder");
    let result = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record match");
    assert_eq!(fixture.player(nonresponder).balance, 0);
    assert!(!result.jc_changes.contains_key(&nonresponder));
}

#[test]
fn test_betting_totals_display_correctly_after_previous_match() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(20_001);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    let bettors = [9_001, 9_002, 9_004, 9_005, 9_003];
    fixture.seed(&bettors);
    for discord_id in bettors {
        fixture
            .repository
            .add_balance(discord_id, Some(GUILD), 20)
            .expect("top up bettor");
    }
    for (id, side, amount) in [
        (9_001, TeamSide::Radiant, 3),
        (9_004, TeamSide::Radiant, 5),
        (9_002, TeamSide::Dire, 2),
        (9_005, TeamSide::Dire, 4),
    ] {
        fixture
            .repository
            .place_bet_atomic(Some(GUILD), id, side, amount, 100)
            .expect("place first bet");
    }
    assert_eq!(
        fixture
            .repository
            .pending_bet_totals(Some(GUILD), 100)
            .expect("totals"),
        PendingBetTotals {
            radiant: 8,
            dire: 6
        }
    );
    let mut first = standard(&radiant, &dire, TeamSide::Radiant);
    first.pending_bet_since = 100;
    fixture
        .repository
        .record_match_atomic(first)
        .expect("settle first match");
    assert_eq!(
        fixture
            .repository
            .pending_bet_totals(Some(GUILD), 100)
            .expect("settled totals"),
        PendingBetTotals::default()
    );
    fixture
        .repository
        .place_bet_atomic(Some(GUILD), 9_003, TeamSide::Dire, 6, 200)
        .expect("new bet");
    assert_eq!(
        fixture
            .repository
            .pending_bet_totals(Some(GUILD), 200)
            .expect("new totals"),
        PendingBetTotals {
            radiant: 0,
            dire: 6
        }
    );
    let bet = fixture
        .repository
        .get_pending_bet(Some(GUILD), 9_003, 200)
        .expect("read bet")
        .expect("pending bet");
    assert_eq!((bet.amount, bet.team), (6, TeamSide::Dire));
}

#[test]
fn test_loan_repaid_when_borrower_wins() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(3_001);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    let borrower = radiant[0];
    fixture
        .repository
        .set_balance(borrower, Some(GUILD), 0)
        .expect("zero borrower");
    assert_eq!(
        fixture
            .repository
            .issue_loan_atomic(borrower, Some(GUILD), 50, 1_000)
            .expect("issue loan"),
        LoanState {
            outstanding_principal: 50,
            outstanding_fee: 10
        }
    );
    let result = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record match");
    assert_eq!(
        fixture
            .repository
            .loan_state(borrower, Some(GUILD))
            .expect("loan state"),
        LoanState::default()
    );
    assert_eq!(result.loan_repayments[0].player_id, borrower);
    assert_eq!(
        (
            result.loan_repayments[0].principal,
            result.loan_repayments[0].fee,
            result.loan_repayments[0].total_repaid
        ),
        (50, 10, 60)
    );
    assert_eq!(
        fixture
            .repository
            .nonprofit_total(Some(GUILD))
            .expect("fund"),
        10
    );
    assert_eq!(fixture.player(borrower).balance, 0);
}

#[test]
fn test_loan_repaid_when_borrower_loses() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(4_001);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    let borrower = dire[0];
    fixture
        .repository
        .set_balance(borrower, Some(GUILD), 0)
        .expect("zero borrower");
    fixture
        .repository
        .issue_loan_atomic(borrower, Some(GUILD), 50, 1_000)
        .expect("issue loan");
    fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record match");
    assert_eq!(
        fixture
            .repository
            .loan_state(borrower, Some(GUILD))
            .expect("loan state"),
        LoanState::default()
    );
    assert_eq!(fixture.player(borrower).balance, -5);
}

#[test]
fn test_loan_with_spectator_bet() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(5_001);
    let spectator = 9_998;
    fixture.seed(&[radiant.clone(), dire.clone(), vec![spectator]].concat());
    fixture
        .repository
        .set_balance(spectator, Some(GUILD), 0)
        .expect("zero spectator");
    fixture
        .repository
        .issue_loan_atomic(spectator, Some(GUILD), 100, 1_000)
        .expect("issue loan");
    fixture
        .repository
        .place_bet_atomic(Some(GUILD), spectator, TeamSide::Radiant, 30, 100)
        .expect("spectator bet");
    let mut first = standard(&radiant, &dire, TeamSide::Radiant);
    first.pending_bet_since = 100;
    fixture
        .repository
        .record_match_atomic(first)
        .expect("first match");
    assert_eq!(
        fixture
            .repository
            .loan_state(spectator, Some(GUILD))
            .expect("outstanding loan"),
        LoanState {
            outstanding_principal: 100,
            outstanding_fee: 20
        }
    );
    assert_eq!(fixture.player(spectator).balance, 130);
    let second_radiant = [radiant[..4].to_vec(), vec![spectator]].concat();
    let second_dire = [radiant[4..].to_vec(), dire[..4].to_vec()].concat();
    fixture
        .repository
        .record_match_atomic(standard(&second_radiant, &second_dire, TeamSide::Radiant))
        .expect("second match");
    assert_eq!(
        fixture
            .repository
            .loan_state(spectator, Some(GUILD))
            .expect("repaid loan"),
        LoanState::default()
    );
    assert_eq!(fixture.player(spectator).balance, 20);
}

#[test]
fn test_multiple_players_with_loans() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(6_001);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    let borrower_one = radiant[0];
    let borrower_two = dire[0];
    for borrower in [borrower_one, borrower_two] {
        fixture
            .repository
            .set_balance(borrower, Some(GUILD), 0)
            .expect("zero borrower");
    }
    fixture
        .repository
        .issue_loan_atomic(borrower_one, Some(GUILD), 50, 1_000)
        .expect("first loan");
    fixture
        .repository
        .issue_loan_atomic(borrower_two, Some(GUILD), 100, 1_000)
        .expect("second loan");
    fixture
        .repository
        .set_balance(borrower_two, Some(GUILD), 0)
        .expect("spend second loan");
    let result = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record match");
    assert_eq!(result.loan_repayments.len(), 2);
    assert_eq!(fixture.player(borrower_one).balance, 0);
    assert_eq!(fixture.player(borrower_two).balance, -115);
}

#[test]
fn test_excluded_player_should_not_get_loss() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(96_001);
    let excluded = 96_011;
    fixture.seed(&[radiant.clone(), dire.clone(), vec![excluded]].concat());
    let mut request = standard(&radiant, &dire, TeamSide::Dire);
    let excluded_ids = [excluded];
    request.rewarded_excluded_ids = &excluded_ids;
    request.all_excluded_ids = &excluded_ids;
    let result = fixture
        .repository
        .record_match_atomic(request)
        .expect("record match");
    let player = fixture.player(excluded);
    assert_eq!((player.wins, player.losses), (0, 0));
    let stored = fixture
        .repository
        .get_match(result.match_id, Some(GUILD))
        .expect("match")
        .expect("stored");
    assert!(!stored.radiant_ids.contains(&excluded) && !stored.dire_ids.contains(&excluded));
    assert!(
        !fixture
            .repository
            .rating_history(result.match_id, Some(GUILD))
            .expect("history")
            .iter()
            .any(|row| row.discord_id == excluded)
    );
}

#[test]
fn test_get_players_by_ids_preserves_order() {
    let fixture = Fixture::new();
    for (discord_id, name, rating) in [
        (1_001, "Alice", 1_500.0),
        (1_002, "Bob", 1_600.0),
        (1_003, "Charlie", 1_700.0),
        (1_004, "Diana", 1_800.0),
        (1_005, "Eve", 1_900.0),
    ] {
        fixture
            .repository
            .add_player(discord_id, Some(GUILD), name, rating)
            .expect("seed named player");
    }
    let players = fixture
        .repository
        .get_players_by_ids(&[1_005, 1_003, 1_001, 1_004, 1_002], Some(GUILD))
        .expect("ordered players");
    assert_eq!(
        players
            .iter()
            .map(|player| player.name.as_str())
            .collect::<Vec<_>>(),
        ["Eve", "Charlie", "Alice", "Diana", "Bob"]
    );
}

#[test]
fn test_team_assignment_with_shuffled_ids() {
    let fixture = Fixture::new();
    let data = [
        (101, "BugReporter"),
        (102, "FakeUser2"),
        (103, "FakeUser7"),
        (104, "TestPlayerA"),
        (105, "FakeUser3"),
        (106, "TestPlayerB"),
        (107, "FakeUser6"),
        (108, "FakeUser4"),
        (109, "FakeUser5"),
        (110, "FakeUser1"),
    ];
    for (discord_id, name) in data {
        fixture
            .repository
            .add_player(discord_id, Some(GUILD), name, 1_500.0)
            .expect("seed player");
    }
    let lobby = [110, 109, 108, 107, 106, 105, 104, 103, 102, 101];
    let players = fixture
        .repository
        .get_players_by_ids(&lobby, Some(GUILD))
        .expect("ordered players");
    let mapping = lobby
        .iter()
        .copied()
        .zip(players.iter().map(|player| player.name.as_str()))
        .map(|(id, name)| (name, id))
        .collect::<BTreeMap<_, _>>();
    let radiant_names = [
        "BugReporter",
        "FakeUser2",
        "FakeUser7",
        "TestPlayerA",
        "FakeUser3",
    ];
    let dire_names = [
        "TestPlayerB",
        "FakeUser6",
        "FakeUser4",
        "FakeUser5",
        "FakeUser1",
    ];
    let radiant = radiant_names.map(|name| mapping[name]).to_vec();
    let dire = dire_names.map(|name| mapping[name]).to_vec();
    assert_eq!(radiant, [101, 102, 103, 104, 105]);
    assert_eq!(dire, [106, 107, 108, 109, 110]);
    fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record assigned teams");
    assert!(
        radiant
            .iter()
            .all(|discord_id| fixture.player(*discord_id).wins == 1)
    );
    assert!(
        dire.iter()
            .all(|discord_id| fixture.player(*discord_id).losses == 1)
    );
    assert_eq!(
        radiant
            .iter()
            .chain(dire.iter())
            .copied()
            .collect::<BTreeSet<_>>()
            .len(),
        10
    );
}

#[test]
fn test_full_workflow_exact_bug_scenario() {
    let fixture = Fixture::new();
    let players = [
        (93_001, "FakeUser917762", 1_405.0),
        (93_002, "FakeUser924119", 1_120.0),
        (93_003, "FakeUser926408", 1_763.0),
        (93_004, "FakeUser921765", 1_689.0),
        (93_005, "FakeUser925589", 1_568.0),
        (93_006, "FakeUser923487", 1_161.0),
        (93_007, "BugReporter", 1_500.0),
        (93_008, "FakeUser921510", 1_816.0),
        (93_009, "FakeUser920053", 1_500.0),
        (93_010, "FakeUser919197", 1_601.0),
    ];
    for &(discord_id, name, rating) in &players {
        fixture
            .repository
            .add_player(discord_id, Some(GUILD), name, rating)
            .expect("seed exact Radiant/Dire regression player");
    }
    let radiant = players[..5]
        .iter()
        .map(|(discord_id, _, _)| *discord_id)
        .collect::<Vec<_>>();
    let dire = players[5..]
        .iter()
        .map(|(discord_id, _, _)| *discord_id)
        .collect::<Vec<_>>();

    let recorded = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Dire))
        .expect("record exact Dire-winner regression");
    assert!(recorded.match_id > 0);

    let reporter = fixture.player(93_007);
    assert_eq!(reporter.name, "BugReporter");
    assert_eq!((reporter.wins, reporter.losses), (1, 0));
    for discord_id in &dire {
        let player = fixture.player(*discord_id);
        assert_eq!((player.wins, player.losses), (1, 0));
    }
    for discord_id in &radiant {
        let player = fixture.player(*discord_id);
        assert_eq!((player.wins, player.losses), (0, 1));
    }

    let mut leaderboard = radiant
        .iter()
        .chain(&dire)
        .map(|discord_id| fixture.player(*discord_id))
        .collect::<Vec<_>>();
    leaderboard.sort_by(|left, right| {
        right
            .wins
            .cmp(&left.wins)
            .then_with(|| right.glicko_rating.total_cmp(&left.glicko_rating))
    });
    assert!(leaderboard[..5].iter().all(|player| player.wins == 1));
    assert!(leaderboard[5..].iter().all(|player| player.wins == 0));
}

#[test]
fn test_full_record_commits_every_dependent_row() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(31_001);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());

    let result = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record complete match core");

    assert!(result.match_id > 0);
    assert_eq!(
        fixture
            .repository
            .get_participants(result.match_id, Some(GUILD))
            .unwrap()
            .len(),
        10
    );
    assert_eq!(
        fixture
            .repository
            .rating_history(result.match_id, Some(GUILD))
            .unwrap()
            .len(),
        10
    );
    let pairings: i64 = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM player_pairings WHERE guild_id = ?1",
            [GUILD],
            |row| row.get(0),
        )
        .expect("pairing count");
    assert_eq!(pairings, 45);
    for discord_id in radiant {
        let player = fixture.player(discord_id);
        assert_eq!((player.wins, player.losses), (1, 0));
    }
    for discord_id in dire {
        let player = fixture.player(discord_id);
        assert_eq!((player.wins, player.losses), (0, 1));
    }
}

#[test]
fn test_exclusion_updates_apply_once_per_pending_match() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(32_001);
    let full_increment_id = 32_011;
    let half_increment_id = 32_012;
    let all_ids = radiant
        .iter()
        .chain(&dire)
        .copied()
        .chain([full_increment_id, half_increment_id])
        .collect::<Vec<_>>();
    fixture.seed(&all_ids);
    let connection = fixture.connection();
    for (discord_id, exclusion_count) in all_ids.iter().copied().zip(0_i64..12) {
        connection
            .execute(
                "UPDATE players SET exclusion_count = ?1
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![exclusion_count, discord_id, GUILD],
            )
            .expect("seed exclusion count");
    }
    drop(connection);
    let read_counts = || {
        let connection = fixture.connection();
        all_ids
            .iter()
            .map(|discord_id| {
                let count = connection
                    .query_row(
                        "SELECT exclusion_count FROM players
                         WHERE discord_id = ?1 AND guild_id = ?2",
                        params![discord_id, GUILD],
                        |row| row.get::<_, i64>(0),
                    )
                    .expect("exclusion count");
                (*discord_id, count)
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let before = read_counts();
    let decay_ids = [&radiant[..], &dire[..]].concat();
    let mut request = standard(&radiant, &dire, TeamSide::Radiant);
    request.pending_match_id = Some(77_001);
    request.exclusion_decay_ids = &decay_ids;
    request.full_exclusion_increment_ids = std::slice::from_ref(&full_increment_id);
    request.half_exclusion_increment_ids = std::slice::from_ref(&half_increment_id);

    let first = fixture
        .repository
        .record_match_atomic(request)
        .expect("first pending record");
    let after_first = read_counts();
    let second = fixture
        .repository
        .record_match_atomic(request)
        .expect("idempotent pending retry");
    let after_second = read_counts();

    assert_eq!(second.match_id, first.match_id);
    for discord_id in radiant.iter().chain(&dire) {
        assert_eq!(after_first[discord_id], (before[discord_id] - 1).max(0));
    }
    assert_eq!(
        after_first[&full_increment_id],
        before[&full_increment_id] + 6
    );
    assert_eq!(
        after_first[&half_increment_id],
        before[&half_increment_id] + 1
    );
    assert_eq!(after_second, after_first);
}

#[test]
fn test_match_and_participants_commit_in_one_call() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(33_001);
    let all_ids = [radiant.clone(), dire.clone()].concat();
    fixture.seed(&all_ids);
    let match_id = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record match")
        .match_id;
    let participant_updates = all_ids
        .iter()
        .map(|discord_id| EnrichmentParticipantUpdate {
            discord_id: *discord_id,
            hero_id: Some(discord_id % 100 + 1),
            kills: Some(5),
            deaths: Some(2),
            assists: Some(8),
            gpm: Some(500),
            xpm: Some(550),
            hero_damage: Some(20_000),
            tower_damage: Some(3_000),
            last_hits: Some(200),
            denies: Some(10),
            net_worth: Some(25_000),
            hero_healing: Some(500),
            lane_role: Some(1),
            lane_efficiency: Some(80),
            gold_at_10: None,
            towers_killed: Some(2),
            roshans_killed: Some(1),
            teamfight_participation: Some(0.75),
            obs_placed: Some(3),
            sen_placed: Some(4),
            camps_stacked: Some(2),
            rune_pickups: Some(5),
            firstblood_claimed: Some(1),
            stuns: Some(8.5),
            fantasy_points: Some(50.0),
        })
        .collect::<Vec<_>>();
    let wrapped_facts = [WrappedEnrichmentFactUpdate {
        discord_id: radiant[0],
        actions_per_min: Some(321),
        courier_kills: Some(2),
        pings: Some(44),
        rapier_count: 2,
        lane_role: Some(2),
        comeback: Some(9_000),
        throw_amount: Some(4_000),
    }];

    let updated = fixture
        .repository
        .apply_enrichment_atomic(MatchEnrichmentRequest {
            match_id,
            guild_id: Some(GUILD),
            valve_match_id: 8_181_518_332,
            duration_seconds: 2_400,
            radiant_score: 35,
            dire_score: 22,
            game_mode: 2,
            enrichment_data: Some(
                r#"{"stub":true,"picks_bans":[{"is_pick":false,"team":0,"hero_id":44},{"is_pick":true,"team":1,"hero_id":55},{"is_pick":0,"team":"1","hero_id":"66"}]}"#,
            ),
            enrichment_source: Some("manual"),
            enrichment_confidence: None,
            participant_updates: &participant_updates,
            wrapped_facts: &wrapped_facts,
        })
        .expect("atomic enrichment");
    assert_eq!(updated, 10);
    let match_values = fixture
        .connection()
        .query_row(
            "SELECT valve_match_id, duration_seconds, radiant_score
             FROM matches WHERE match_id = ?1 AND guild_id = ?2",
            params![match_id, GUILD],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .expect("match enrichment");
    assert_eq!(match_values, (8_181_518_332, 2_400, 35));
    let wrapped_values = fixture
        .connection()
        .query_row(
            "SELECT actions_per_min, rapier_count, comeback, throw
               FROM wrapped_enrichment_facts
              WHERE guild_id = ?1 AND match_id = ?2 AND discord_id = ?3",
            params![GUILD, match_id, radiant[0]],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .expect("wrapped enrichment fact");
    assert_eq!(wrapped_values, (321, 2, 9_000, 4_000));
    let bans = fixture
        .connection()
        .prepare(
            "SELECT ban_index, team, hero_id FROM match_bans
             WHERE match_id = ?1 ORDER BY ban_index",
        )
        .and_then(|mut statement| {
            statement
                .query_map([match_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .expect("Scout ban projection");
    assert_eq!(bans, [(0, 0, 44), (2, 1, 66)]);
    let populated: i64 = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM match_participants
             WHERE match_id = ?1 AND hero_id IS NOT NULL AND fantasy_points = 50.0",
            [match_id],
            |row| row.get(0),
        )
        .expect("participant enrichment count");
    assert_eq!(populated, 10);
    let replay_reason: String = fixture
        .connection()
        .query_row(
            "SELECT reason FROM openskill_replay_jobs WHERE guild_id = ?1",
            [GUILD],
            |row| row.get(0),
        )
        .expect("durable OpenSkill replay request");
    assert_eq!(replay_reason, format!("match_enrichment:{match_id}"));
}

#[test]
fn test_extract_match_bans_keeps_only_valid_bans() {
    let fixture = Fixture::new();
    let match_id = recorded_enrichment_match(&fixture, 34_001);

    assert_eq!(
        apply_enrichment_payload(
            &fixture,
            match_id,
            r#"{
                "picks_bans": [
                    {"is_pick": false, "team": 1, "hero_id": 10},
                    {"is_pick": true, "team": 0, "hero_id": 20},
                    {"is_pick": false, "team": 3, "hero_id": 30},
                    {"is_pick": false, "team": 0, "hero_id": 0},
                    "corrupt",
                    {"is_pick": 0, "team": "0", "hero_id": "40"},
                    {"is_pick": 0.0, "team": 0.9, "hero_id": 50.9},
                    {"is_pick": false, "team": " 1 ", "hero_id": " 6_000 "},
                    {"is_pick": false, "team": false, "hero_id": true}
                ]
            }"#,
            "manual",
        ),
        0
    );
    assert_eq!(
        projected_bans(&fixture, match_id),
        [(0, 1, 10), (5, 0, 40), (6, 0, 50), (7, 1, 6_000)]
    );

    assert_eq!(
        apply_enrichment_payload(&fixture, match_id, "{not-json", "manual"),
        0
    );
    assert!(projected_bans(&fixture, match_id).is_empty());
}

#[test]
fn test_reenrichment_replaces_normalized_bans() {
    let fixture = Fixture::new();
    let match_id = recorded_enrichment_match(&fixture, 35_001);
    apply_enrichment_payload(
        &fixture,
        match_id,
        r#"{"picks_bans":[
            {"is_pick":false,"team":1,"hero_id":10},
            {"is_pick":false,"team":1,"hero_id":20}
        ]}"#,
        "manual",
    );
    assert_eq!(projected_bans(&fixture, match_id), [(0, 1, 10), (1, 1, 20)]);

    apply_enrichment_payload(
        &fixture,
        match_id,
        r#"{"picks_bans":[
            {"is_pick":false,"team":0,"hero_id":30},
            {"is_pick":false,"team":1,"hero_id":40}
        ]}"#,
        "manual",
    );
    assert_eq!(projected_bans(&fixture, match_id), [(0, 0, 30), (1, 1, 40)]);
}

#[test]
fn test_atomic_enrichment_projects_bans() {
    let fixture = Fixture::new();
    let match_id = recorded_enrichment_match(&fixture, 36_001);

    assert_eq!(
        apply_enrichment_payload(
            &fixture,
            match_id,
            r#"{"picks_bans":[{"is_pick":false,"team":1,"hero_id":10}]}"#,
            "auto",
        ),
        0
    );
    assert_eq!(projected_bans(&fixture, match_id), [(0, 1, 10)]);
}

#[test]
fn test_atomic_write_wipe_and_reenrich_keep_compact_facts_consistent() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(37_001);
    let participants = [radiant.clone(), dire.clone()].concat();
    fixture.seed(&participants);
    let match_id = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record compact-facts match")
        .match_id;
    let initial = [
        WrappedEnrichmentFactUpdate {
            discord_id: radiant[0],
            actions_per_min: Some(321),
            courier_kills: Some(2),
            pings: Some(44),
            rapier_count: 2,
            lane_role: Some(2),
            comeback: Some(9_000),
            throw_amount: Some(4_000),
        },
        WrappedEnrichmentFactUpdate {
            discord_id: dire[0],
            actions_per_min: None,
            courier_kills: None,
            pings: None,
            rapier_count: 0,
            lane_role: None,
            comeback: Some(9_000),
            throw_amount: Some(4_000),
        },
        WrappedEnrichmentFactUpdate {
            discord_id: 999_999,
            actions_per_min: Some(9_999),
            courier_kills: None,
            pings: None,
            rapier_count: 9,
            lane_role: None,
            comeback: None,
            throw_amount: None,
        },
    ];
    fixture
        .repository
        .apply_enrichment_atomic(MatchEnrichmentRequest {
            match_id,
            guild_id: Some(GUILD),
            valve_match_id: 8_000_000_001,
            duration_seconds: 2_400,
            radiant_score: 30,
            dire_score: 20,
            game_mode: 2,
            enrichment_data: Some(r#"{"players":[]}"#),
            enrichment_source: Some("manual"),
            enrichment_confidence: None,
            participant_updates: &[],
            wrapped_facts: &initial,
        })
        .expect("write initial compact projection");
    assert_eq!(
        projected_wrapped_facts(&fixture, match_id),
        [
            (
                radiant[0],
                Some(321),
                Some(2),
                Some(44),
                2,
                Some(2),
                Some(9_000),
                Some(4_000),
            ),
            (dire[0], None, None, None, 0, None, Some(9_000), Some(4_000),),
        ]
    );

    let discovery =
        crate::match_discovery_repository::MatchDiscoveryRepository::new(fixture.file.path());
    assert!(
        !discovery
            .wipe_match_enrichment(match_id, Some(GUILD + 1))
            .expect("other guild cannot wipe projection")
    );
    assert_eq!(projected_wrapped_facts(&fixture, match_id).len(), 2);
    assert!(
        discovery
            .wipe_match_enrichment(match_id, Some(GUILD))
            .expect("owning guild wipes projection")
    );
    assert!(projected_wrapped_facts(&fixture, match_id).is_empty());

    let replacement = [
        WrappedEnrichmentFactUpdate {
            discord_id: radiant[0],
            actions_per_min: Some(999),
            courier_kills: Some(2),
            pings: Some(44),
            rapier_count: 2,
            lane_role: Some(2),
            comeback: Some(123),
            throw_amount: Some(456),
        },
        WrappedEnrichmentFactUpdate {
            discord_id: dire[0],
            actions_per_min: None,
            courier_kills: None,
            pings: None,
            rapier_count: 0,
            lane_role: None,
            comeback: Some(123),
            throw_amount: Some(456),
        },
    ];
    fixture
        .repository
        .apply_enrichment_atomic(MatchEnrichmentRequest {
            match_id,
            guild_id: Some(GUILD),
            valve_match_id: 8_000_000_002,
            duration_seconds: 2_400,
            radiant_score: 30,
            dire_score: 20,
            game_mode: 2,
            enrichment_data: Some(r#"{"players":[]}"#),
            enrichment_source: Some("manual"),
            enrichment_confidence: None,
            participant_updates: &[],
            wrapped_facts: &replacement,
        })
        .expect("write replacement projection");
    assert_eq!(
        projected_wrapped_facts(&fixture, match_id),
        [
            (
                radiant[0],
                Some(999),
                Some(2),
                Some(44),
                2,
                Some(2),
                Some(123),
                Some(456),
            ),
            (dire[0], None, None, None, 0, None, Some(123), Some(456),),
        ]
    );
}

#[test]
fn test_complete_enrichment_marks_replay_pending_in_same_transaction() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(38_001);
    let participants = [radiant.clone(), dire.clone()].concat();
    fixture.seed(&participants);
    let match_id = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record replay-marker match")
        .match_id;
    let updates = participants
        .iter()
        .enumerate()
        .map(|(index, discord_id)| fantasy_update(*discord_id, 10.0 + index as f64))
        .collect::<Vec<_>>();
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_replay_marker
             BEFORE INSERT ON openskill_replay_jobs BEGIN
               SELECT RAISE(ABORT,'injected replay marker failure');
             END;",
        )
        .expect("install marker failure trigger");
    let failed = fixture
        .repository
        .apply_enrichment_atomic(MatchEnrichmentRequest {
            match_id,
            guild_id: Some(GUILD),
            valve_match_id: 777,
            duration_seconds: 2_400,
            radiant_score: 30,
            dire_score: 20,
            game_mode: 22,
            enrichment_data: None,
            enrichment_source: Some("test"),
            enrichment_confidence: Some(1.0),
            participant_updates: &updates,
            wrapped_facts: &[],
        });
    assert!(failed.is_err());
    let connection = fixture.connection();
    let state: (Option<i64>, i64, i64) = connection
        .query_row(
            "SELECT m.valve_match_id,
                    (SELECT COUNT(*) FROM match_participants
                      WHERE match_id=m.match_id AND fantasy_points IS NOT NULL),
                    (SELECT COUNT(*) FROM openskill_replay_jobs WHERE guild_id=?2)
               FROM matches AS m WHERE m.match_id=?1",
            params![match_id, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("inspect rolled-back enrichment");
    assert_eq!(state, (None, 0, 0));
    connection
        .execute("DROP TRIGGER fail_replay_marker", [])
        .expect("remove marker failure trigger");
    drop(connection);

    assert_eq!(
        fixture
            .repository
            .apply_enrichment_atomic(MatchEnrichmentRequest {
                match_id,
                guild_id: Some(GUILD),
                valve_match_id: 777,
                duration_seconds: 2_400,
                radiant_score: 30,
                dire_score: 20,
                game_mode: 22,
                enrichment_data: None,
                enrichment_source: Some("test"),
                enrichment_confidence: Some(1.0),
                participant_updates: &updates,
                wrapped_facts: &[],
            })
            .expect("commit complete enrichment"),
        10
    );
    let reason: String = fixture
        .connection()
        .query_row(
            "SELECT reason FROM openskill_replay_jobs WHERE guild_id=?1",
            [GUILD],
            |row| row.get(0),
        )
        .expect("durable replay marker");
    assert_eq!(reason, format!("match_enrichment:{match_id}"));
}

fn migrated_award_database() -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary migrated award database");
    initialize_or_migrate(database.path()).expect("initialize Python-compatible schema");
    database
}

fn insert_award_player(connection: &Connection, discord_id: i64, balance: i64) {
    connection
        .execute_batch("PRAGMA foreign_keys=OFF")
        .expect("disable unrelated legacy foreign keys in fixture");
    connection
        .execute(
            "INSERT INTO players (
                 discord_id,guild_id,discord_username,jopacoin_balance
             ) VALUES (?1,?2,?3,?4)",
            params![discord_id, GUILD, format!("award-{discord_id}"), balance],
        )
        .expect("insert award player");
}

#[test]
fn test_balance_add_in_guild1_does_not_affect_guild2() {
    const OTHER_GUILD: i64 = 9_002;
    const PLAYER: i64 = 70_000;
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open guild-scoped balance fixture");
    insert_award_player(&connection, PLAYER, 200);
    connection
        .execute(
            "INSERT INTO players (
                 discord_id,guild_id,discord_username,jopacoin_balance
             ) VALUES (?1,?2,?3,200)",
            params![PLAYER, OTHER_GUILD, "award-other-guild"],
        )
        .expect("insert same player in other guild");
    drop(connection);

    MatchRecordingRepository::new(database.path())
        .add_balance(PLAYER, Some(GUILD), 50)
        .expect("credit only requested guild");

    let connection = Connection::open(database.path()).expect("inspect guild-scoped balance");
    let balances = connection
        .prepare(
            "SELECT guild_id,jopacoin_balance FROM players
             WHERE discord_id=?1 ORDER BY guild_id",
        )
        .expect("prepare guild balances")
        .query_map([PLAYER], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("read guild balances")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect guild balances");
    assert_eq!(balances, vec![(GUILD, 250), (OTHER_GUILD, 200)]);
}

#[test]
fn test_win_bonus_in_guild1_does_not_touch_guild2() {
    const OTHER_GUILD: i64 = 9_002;
    const PLAYER: i64 = 70_000;
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open guild-scoped award fixture");
    insert_award_player(&connection, PLAYER, 200);
    connection
        .execute(
            "INSERT INTO players (
                 discord_id,guild_id,discord_username,jopacoin_balance
             ) VALUES (?1,?2,?3,200)",
            params![PLAYER, OTHER_GUILD, "award-other-guild"],
        )
        .expect("insert same player in other guild");
    drop(connection);

    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest {
                discord_id: PLAYER,
                gross: 20,
                source: "match_win_bonus",
                related_type: Some("match_win_bonus"),
                related_id: Some(912),
                reason: "match win bonus",
                exact_once: true,
                ..IncomeAwardRequest::ordinary(PLAYER, 20, "match_win_bonus")
            }],
        )
        .expect("credit match win only in requested guild");
    assert_eq!(receipt[0].balance_after, 220);

    let connection = Connection::open(database.path()).expect("inspect guild-scoped award");
    let balances = connection
        .prepare(
            "SELECT guild_id,jopacoin_balance FROM players
             WHERE discord_id=?1 ORDER BY guild_id",
        )
        .expect("prepare guild balances")
        .query_map([PLAYER], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("read guild balances")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect guild balances");
    assert_eq!(balances, vec![(GUILD, 220), (OTHER_GUILD, 200)]);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                 WHERE guild_id=?1 AND account_id=?2
                   AND source='match_win_bonus' AND related_id='912'",
                params![OTHER_GUILD, PLAYER],
                |row| row.get::<_, i64>(0),
            )
            .expect("count cross-guild match-win ledger rows"),
        0
    );
}

#[test]
fn canonical_migrated_sqlite_income_order_and_match_marker_match_python() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open migrated award database");
    insert_award_player(&connection, 70_001, -1_000);
    connection
        .execute(
            "INSERT INTO bankruptcy_state (
                 discord_id,guild_id,last_bankruptcy_at,
                 penalty_games_remaining,bankruptcy_count
             ) VALUES (?1,?2,0,2,1)",
            params![70_001, GUILD],
        )
        .expect("insert bankruptcy state");
    drop(connection);

    let repository = MatchRecordingRepository::new(database.path());
    let award = IncomeAwardRequest {
        discord_id: 70_001,
        gross: 100,
        garnishment_rate: 0.5,
        apply_bankruptcy_penalty: true,
        bankruptcy_kept_rate: 0.75,
        vanity_tax_rate: 0.10,
        decrement_bankruptcy_penalty: true,
        source: "match_win_bonus",
        related_type: Some("match_win_bonus"),
        related_id: Some(811),
        reason: "match win bonus",
        exact_once: true,
    };
    let first = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("credit canonical award");
    assert_eq!(
        first,
        vec![IncomeAwardReceipt {
            discord_id: 70_001,
            gross: 100,
            garnished: 50,
            bankruptcy_penalty: 12,
            vanity_tax: 5,
            net: 33,
            balance_delta: 83,
            balance_after: -917,
            penalty_games_remaining: 1,
            applied: true,
        }]
    );

    let retry = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("retry exact-once award");
    assert_eq!(retry[0].balance_delta, 83);
    assert!(!retry[0].applied);
    let connection = Connection::open(database.path()).expect("reopen migrated award database");
    let state: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT p.jopacoin_balance,b.penalty_games_remaining,
                    (SELECT COALESCE(SUM(delta),0) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND account_id=?2
                       AND related_type='match_win_bonus' AND related_id='811'),
                    (SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND account_id=?2
                       AND related_type='match_win_bonus' AND related_id='811')
             FROM players p JOIN bankruptcy_state b
               ON b.discord_id=p.discord_id AND b.guild_id=p.guild_id
             WHERE p.guild_id=?1 AND p.discord_id=?2",
            params![GUILD, 70_001],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read canonical award state");
    assert_eq!(state, (-917, 1, 83, 3));
}

#[test]
fn exact_once_income_marker_and_recovery_are_isolated_by_source() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open migrated award database");
    insert_award_player(&connection, 70_002, 0);
    drop(connection);

    let repository = MatchRecordingRepository::new(database.path());
    let participation = IncomeAwardRequest {
        discord_id: 70_002,
        gross: 100,
        vanity_tax_rate: 0.10,
        source: "match_participation",
        related_type: Some("match"),
        related_id: Some(812),
        reason: "match participation bonus",
        exact_once: true,
        ..IncomeAwardRequest::ordinary(70_002, 100, "match_participation")
    };
    let streaming = IncomeAwardRequest {
        gross: 50,
        source: "match_streaming_bonus",
        reason: "match streaming bonus",
        ..participation
    };
    let first = repository
        .credit_income_awards_atomic(Some(GUILD), &[participation, streaming])
        .expect("credit two independent match awards");
    assert_eq!(first[0].balance_delta, 90);
    assert_eq!(first[1].balance_delta, 45);
    assert!(first.iter().all(|receipt| receipt.applied));

    let retry = repository
        .credit_income_awards_atomic(Some(GUILD), &[streaming, participation])
        .expect("recover each award by its source marker");
    assert_eq!(retry[0].balance_delta, 45);
    assert_eq!(retry[0].vanity_tax, 5);
    assert_eq!(retry[1].balance_delta, 90);
    assert_eq!(retry[1].vanity_tax, 10);
    assert!(retry.iter().all(|receipt| !receipt.applied));
    let balance: i64 = Connection::open(database.path())
        .expect("reopen exact-once database")
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![70_002, GUILD],
            |row| row.get(0),
        )
        .expect("read exactly-once balance");
    assert_eq!(balance, 135);
}

#[test]
fn live_migrated_sqlite_batch_rechecks_debt_and_rolls_back_as_one_unit() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open migrated award database");
    insert_award_player(&connection, 70_101, -5);
    drop(connection);
    let repository = MatchRecordingRepository::new(database.path());
    let award = IncomeAwardRequest {
        garnishment_rate: 0.5,
        reason: "participation reward",
        ..IncomeAwardRequest::ordinary(70_101, 10, "match_participation")
    };
    let receipts = repository
        .credit_income_awards_atomic(Some(GUILD), &[award, award])
        .expect("credit ordered live-balance awards");
    assert_eq!(receipts[0].garnished, 5);
    assert_eq!(receipts[1].garnished, 0);
    assert_eq!(receipts[1].balance_after, 15);

    let missing = IncomeAwardRequest::ordinary(99_999, 10, "match_participation");
    let failed = repository.credit_income_awards_atomic(Some(GUILD), &[award, missing]);
    assert!(matches!(
        failed,
        Err(MatchRecordingRepositoryError::MissingPlayer(99_999))
    ));
    let balance: i64 = Connection::open(database.path())
        .expect("reopen rolled-back award database")
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![70_101, GUILD],
            |row| row.get(0),
        )
        .expect("read balance after rollback");
    assert_eq!(balance, 15);
}

#[test]
fn penalty_base_uses_live_positive_balance_without_stale_garnishment_peek() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open positive penalty fixture");
    insert_award_player(&connection, 70_111, 50);
    connection
        .execute(
            "INSERT INTO bankruptcy_state(
                 discord_id,guild_id,last_bankruptcy_at,penalty_games_remaining,bankruptcy_count
             ) VALUES (?1,?2,0,3,1)",
            params![70_111, GUILD],
        )
        .expect("positive penalty state");
    drop(connection);
    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest {
                garnishment_rate: 0.5,
                apply_bankruptcy_penalty: true,
                bankruptcy_kept_rate: 0.5,
                ..IncomeAwardRequest::ordinary(70_111, 10, "match_participation")
            }],
        )
        .expect("positive live penalty award")[0];
    assert_eq!(
        (receipt.garnished, receipt.bankruptcy_penalty, receipt.net),
        (0, 5, 5)
    );
}

#[test]
fn penalty_base_uses_atomic_garnished_net_while_player_is_in_debt() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open debt penalty fixture");
    insert_award_player(&connection, 70_112, -100);
    connection
        .execute(
            "INSERT INTO bankruptcy_state(
                 discord_id,guild_id,last_bankruptcy_at,penalty_games_remaining,bankruptcy_count
             ) VALUES (?1,?2,0,3,1)",
            params![70_112, GUILD],
        )
        .expect("debt penalty state");
    drop(connection);
    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest {
                garnishment_rate: 0.5,
                apply_bankruptcy_penalty: true,
                bankruptcy_kept_rate: 0.5,
                ..IncomeAwardRequest::ordinary(70_112, 10, "match_participation")
            }],
        )
        .expect("debt live penalty award")[0];
    assert_eq!(
        (receipt.garnished, receipt.bankruptcy_penalty, receipt.net),
        (5, 2, 3)
    );
}

#[test]
fn race_fix3_participation_without_garnishment_is_atomic() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open race participation fixture");
    insert_award_player(&connection, 70_113, 0);
    drop(connection);
    let repository = MatchRecordingRepository::new(database.path());
    let award = IncomeAwardRequest::ordinary(70_113, 10, "match_participation");
    let missing = IncomeAwardRequest::ordinary(70_999, 10, "match_participation");
    assert!(matches!(
        repository.credit_income_awards_atomic(Some(GUILD), &[award, missing]),
        Err(MatchRecordingRepositoryError::MissingPlayer(70_999))
    ));
    let balance: i64 = Connection::open(database.path())
        .expect("reopen race participation fixture")
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE guild_id=?1 AND discord_id=?2",
            params![GUILD, 70_113],
            |row| row.get(0),
        )
        .expect("read rolled-back participation");
    assert_eq!(balance, 0);
}

#[test]
fn race_fix3_participation_reports_net_without_garnishment_service() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open race net fixture");
    insert_award_player(&connection, 70_114, 20);
    drop(connection);
    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest::ordinary(
                70_114,
                10,
                "match_participation",
            )],
        )
        .expect("no-garnishment participation")[0];
    assert_eq!((receipt.gross, receipt.garnished, receipt.net), (10, 0, 10));
}

#[test]
fn race_fix4_garnishment_runs_before_bankruptcy_penalty() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open race ordering fixture");
    insert_award_player(&connection, 70_115, -100);
    connection
        .execute(
            "INSERT INTO bankruptcy_state(
                 discord_id,guild_id,last_bankruptcy_at,penalty_games_remaining,bankruptcy_count
             ) VALUES (?1,?2,0,3,1)",
            params![70_115, GUILD],
        )
        .expect("race ordering bankruptcy");
    drop(connection);
    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest {
                garnishment_rate: 0.5,
                apply_bankruptcy_penalty: true,
                bankruptcy_kept_rate: 0.5,
                ..IncomeAwardRequest::ordinary(70_115, 10, "match_win_bonus")
            }],
        )
        .expect("ordered garnishment and penalty")[0];
    assert_eq!(
        (receipt.garnished, receipt.bankruptcy_penalty, receipt.net),
        (5, 2, 3)
    );
}

#[test]
fn race_fix4_garnishment_matches_positive_balance_ordering() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open race positive fixture");
    insert_award_player(&connection, 70_116, 100);
    drop(connection);
    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest {
                garnishment_rate: 0.75,
                ..IncomeAwardRequest::ordinary(70_116, 10, "match_participation")
            }],
        )
        .expect("positive ordering")[0];
    assert_eq!((receipt.garnished, receipt.net), (0, 10));
}

fn ordinary_garnishment_receipt(
    balance: i64,
    gross: i64,
    rate: f64,
    guild_id: i64,
) -> (NamedTempFile, IncomeAwardReceipt) {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open garnishment database");
    insert_award_player(&connection, 71_001, balance);
    drop(connection);
    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(guild_id),
            &[IncomeAwardRequest {
                garnishment_rate: rate,
                ..IncomeAwardRequest::ordinary(71_001, gross, "participation")
            }],
        )
        .expect("credit garnishable income")
        .into_iter()
        .next()
        .expect("one garnishment receipt");
    (database, receipt)
}

#[test]
fn test_no_garnishment_when_positive_balance() {
    let (_, receipt) = ordinary_garnishment_receipt(3, 100, 1.0, GUILD);
    assert_eq!(
        (receipt.gross, receipt.garnished, receipt.net),
        (100, 0, 100)
    );
    assert_eq!(receipt.balance_after, 103);
}

#[test]
fn test_full_garnishment_when_in_debt() {
    let (_, receipt) = ordinary_garnishment_receipt(-50, 30, 1.0, GUILD);
    assert_eq!((receipt.gross, receipt.garnished, receipt.net), (30, 30, 0));
    assert_eq!(receipt.balance_after, -20);
}

#[test]
fn test_partial_garnishment_with_50_percent_rate() {
    let (_, receipt) = ordinary_garnishment_receipt(-100, 40, 0.5, GUILD);
    assert_eq!(
        (receipt.gross, receipt.garnished, receipt.net),
        (40, 20, 20)
    );
    assert_eq!(receipt.balance_after, -60);
}

#[test]
fn test_zero_income_no_garnishment() {
    let (_, receipt) = ordinary_garnishment_receipt(-50, 0, 1.0, GUILD);
    assert_eq!((receipt.gross, receipt.garnished, receipt.net), (0, 0, 0));
    assert_eq!(receipt.balance_after, -50);
}

#[test]
fn test_negative_income_no_garnishment() {
    let (_, receipt) = ordinary_garnishment_receipt(-50, -20, 1.0, GUILD);
    assert_eq!(
        (receipt.gross, receipt.garnished, receipt.net),
        (-20, 0, -20)
    );
    assert_eq!(receipt.balance_after, -50);
}

#[test]
fn test_income_pays_off_debt_completely() {
    let (_, receipt) = ordinary_garnishment_receipt(-30, 50, 1.0, GUILD);
    assert_eq!((receipt.gross, receipt.garnished, receipt.net), (50, 50, 0));
    assert_eq!(receipt.balance_after, 20);
}

#[test]
fn test_exactly_at_zero_balance() {
    let (_, receipt) = ordinary_garnishment_receipt(0, 100, 1.0, GUILD);
    assert_eq!(
        (receipt.gross, receipt.garnished, receipt.net),
        (100, 0, 100)
    );
    assert_eq!(receipt.balance_after, 100);
}

#[test]
fn test_custom_garnishment_rate() {
    let (_, receipt) = ordinary_garnishment_receipt(-100, 40, 0.75, GUILD);
    assert_eq!((receipt.garnished, receipt.net), (30, 10));
}

#[test]
fn leveraged_bet_income_is_garnished_while_in_debt() {
    let (_, receipt) = ordinary_garnishment_receipt(-100, 20, 0.5, GUILD);
    assert_eq!(
        (receipt.gross, receipt.garnished, receipt.net),
        (20, 10, 10)
    );
    assert_eq!(receipt.balance_after, -80);
}

#[test]
fn leveraged_bet_income_skips_garnishment_with_positive_balance() {
    let (_, receipt) = ordinary_garnishment_receipt(10, 20, 0.5, GUILD);
    assert_eq!((receipt.gross, receipt.garnished, receipt.net), (20, 0, 20));
    assert_eq!(receipt.balance_after, 30);
}

#[test]
fn leveraged_bet_income_uses_configured_garnishment_service_rate() {
    let (_, receipt) = ordinary_garnishment_receipt(-50, 10, 0.75, GUILD);
    assert_eq!((receipt.gross, receipt.garnished, receipt.net), (10, 7, 3));
    assert_eq!(receipt.balance_after, -40);
}

#[test]
fn test_zero_garnishment_rate() {
    let (_, receipt) = ordinary_garnishment_receipt(-100, 50, 0.0, GUILD);
    assert_eq!((receipt.garnished, receipt.net), (0, 50));
    assert_eq!(receipt.balance_after, -50);
}

#[test]
fn test_bulk_income_uses_one_transaction_and_live_duplicate_balance() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open bulk garnishment database");
    insert_award_player(&connection, 71_001, -3);
    drop(connection);
    let award = IncomeAwardRequest {
        garnishment_rate: 0.5,
        ..IncomeAwardRequest::ordinary(71_001, 4, "participation")
    };
    let receipts = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(Some(GUILD), &[award, award])
        .expect("credit duplicate player in one transaction");
    assert_eq!(
        receipts
            .iter()
            .map(|receipt| receipt.garnished)
            .collect::<Vec<_>>(),
        [2, 0]
    );
    assert_eq!(
        receipts
            .iter()
            .map(|receipt| receipt.net)
            .collect::<Vec<_>>(),
        [2, 4]
    );
    assert_eq!(receipts[1].balance_after, 5);
}

#[test]
fn test_bulk_income_rolls_back_all_players_on_error() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open rollback garnishment database");
    insert_award_player(&connection, 71_001, 3);
    drop(connection);
    let awards = [
        IncomeAwardRequest::ordinary(71_001, 10, "participation"),
        IncomeAwardRequest::ordinary(999_999, 10, "participation"),
    ];
    assert!(matches!(
        MatchRecordingRepository::new(database.path())
            .credit_income_awards_atomic(Some(GUILD), &awards),
        Err(MatchRecordingRepositoryError::MissingPlayer(999_999))
    ));
    let balance: i64 = Connection::open(database.path())
        .expect("reopen rollback garnishment database")
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id=71001 AND guild_id=?1",
            [GUILD],
            |row| row.get(0),
        )
        .expect("read rolled-back garnishment balance");
    assert_eq!(balance, 3);
}

#[test]
fn test_garnishment_uses_correct_guild_balance() {
    const OTHER_GUILD: i64 = 9_002;
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open guild garnishment database");
    insert_award_player(&connection, 71_001, -50);
    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance)
             VALUES(71001,?1,'other-guild',100)",
            [OTHER_GUILD],
        )
        .expect("insert other-guild player");
    drop(connection);
    let repository = MatchRecordingRepository::new(database.path());
    let award = IncomeAwardRequest {
        garnishment_rate: 1.0,
        ..IncomeAwardRequest::ordinary(71_001, 30, "participation")
    };
    let indebted = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("credit indebted guild");
    let positive = repository
        .credit_income_awards_atomic(Some(OTHER_GUILD), &[award])
        .expect("credit positive guild");
    assert_eq!(indebted[0].garnished, 30);
    assert_eq!(positive[0].garnished, 0);
}

/// (lane_role, gold_at_10) for a standard lineup, applied to each side.
const ENRICHMENT_LANES: [(i64, i64); 5] =
    [(1, 4_000), (2, 3_800), (3, 3_200), (3, 1_800), (1, 1_500)];

fn lane_participant_updates(all_ids: &[i64]) -> Vec<EnrichmentParticipantUpdate> {
    all_ids
        .iter()
        .enumerate()
        .map(|(index, discord_id)| {
            let (lane_role, gold_at_10) = ENRICHMENT_LANES[index % 5];
            EnrichmentParticipantUpdate {
                discord_id: *discord_id,
                hero_id: Some(discord_id % 100 + 1),
                kills: None,
                deaths: None,
                assists: None,
                gpm: None,
                xpm: None,
                hero_damage: None,
                tower_damage: None,
                last_hits: None,
                denies: None,
                net_worth: None,
                hero_healing: None,
                lane_role: Some(lane_role),
                lane_efficiency: None,
                gold_at_10: Some(gold_at_10),
                towers_killed: None,
                roshans_killed: None,
                teamfight_participation: None,
                obs_placed: None,
                sen_placed: None,
                camps_stacked: None,
                rune_pickups: None,
                firstblood_claimed: None,
                stuns: None,
                fantasy_points: None,
            }
        })
        .collect()
}

fn enrich_with_lanes(
    fixture: &Fixture,
    match_id: i64,
    updates: &[EnrichmentParticipantUpdate],
) -> usize {
    fixture
        .repository
        .apply_enrichment_atomic(MatchEnrichmentRequest {
            match_id,
            guild_id: Some(GUILD),
            valve_match_id: 9_000_000_001,
            duration_seconds: 2_100,
            radiant_score: 30,
            dire_score: 20,
            game_mode: 2,
            enrichment_data: Some(r#"{"stub":true}"#),
            enrichment_source: Some("manual"),
            enrichment_confidence: None,
            participant_updates: updates,
            wrapped_facts: &[],
        })
        .expect("atomic enrichment")
}

fn stored_derived_roles(fixture: &Fixture, match_id: i64) -> Vec<(i64, Option<String>)> {
    fixture
        .connection()
        .prepare(
            "SELECT discord_id, derived_role FROM match_participants
              WHERE match_id = ?1 AND guild_id = ?2 ORDER BY discord_id",
        )
        .expect("prepare derived read")
        .query_map(params![match_id, GUILD], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("query derived roles")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect derived roles")
}

#[test]
fn test_enrichment_derives_played_roles_in_the_same_transaction() {
    let fixture = Fixture::new();
    let radiant: Vec<i64> = (0..5).map(|index| 4_100 + index).collect();
    let dire: Vec<i64> = (0..5).map(|index| 4_200 + index).collect();
    let all_ids = [radiant.clone(), dire.clone()].concat();
    fixture.seed(&all_ids);
    let match_id = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record match")
        .match_id;

    // New matches must get a played role without waiting for a backfill pass.
    enrich_with_lanes(&fixture, match_id, &lane_participant_updates(&all_ids));

    let roles = stored_derived_roles(&fixture, match_id);
    let by_id: std::collections::BTreeMap<i64, Option<String>> = roles.into_iter().collect();
    for base in [4_100_i64, 4_200] {
        assert_eq!(by_id[&base], Some("1".to_owned()));
        assert_eq!(by_id[&(base + 1)], Some("2".to_owned()));
        assert_eq!(by_id[&(base + 2)], Some("3".to_owned()));
        assert_eq!(by_id[&(base + 3)], Some("4".to_owned()));
        assert_eq!(by_id[&(base + 4)], Some("5".to_owned()));
    }
}

#[test]
fn test_enrichment_leaves_roles_null_when_lanes_are_ambiguous() {
    let fixture = Fixture::new();
    let radiant: Vec<i64> = (0..5).map(|index| 4_300 + index).collect();
    let dire: Vec<i64> = (0..5).map(|index| 4_400 + index).collect();
    let all_ids = [radiant.clone(), dire.clone()].concat();
    fixture.seed(&all_ids);
    let match_id = fixture
        .repository
        .record_match_atomic(standard(&radiant, &dire, TeamSide::Radiant))
        .expect("record match")
        .match_id;

    // Two mids on every side: unresolvable, so nothing is written rather than
    // a guess being stored.
    let mut updates = lane_participant_updates(&all_ids);
    for update in &mut updates {
        update.lane_role = Some(2);
    }
    enrich_with_lanes(&fixture, match_id, &updates);

    assert!(
        stored_derived_roles(&fixture, match_id)
            .iter()
            .all(|(_, role)| role.is_none())
    );
}
