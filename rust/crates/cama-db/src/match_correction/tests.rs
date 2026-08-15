use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Barrier};
use std::thread;

use cama_domain::openskill::{CamaOpenSkillSystem, Player as OpenSkillPlayer, WinningTeam};
use cama_domain::rating::{CamaRatingSystem, MatchUpdateOptions, RatingConfig, TeamPlayer};
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
const GUILD: i64 = 7_777;
const ADMIN: i64 = 88_888;
const WIN_REWARD: i64 = 10;
const PARTICIPATION_REWARD: i64 = 5;
const NOW: i64 = 1_786_051_200;

struct Fixture {
    file: NamedTempFile,
    repository: MatchCorrectionRepository,
}

#[derive(Clone, Debug)]
struct SeededMatch {
    match_id: i64,
    radiant_ids: Vec<i64>,
    dire_ids: Vec<i64>,
}

impl Fixture {
    fn new() -> Self {
        let file = NamedTempFile::new().expect("temporary match correction database");
        let connection = Connection::open(file.path()).expect("open correction fixture");
        connection
            .execute_batch(FIXTURE_SCHEMA)
            .expect("create Python-compatible disposable schema");
        drop(connection);
        Self {
            repository: MatchCorrectionRepository::new(file.path()),
            file,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.file.path()).expect("open correction fixture")
    }

    fn seed_match(&self, start_id: i64) -> SeededMatch {
        self.seed_match_at(start_id, NOW, MatchSide::Radiant)
    }

    fn seed_match_at(&self, start_id: i64, match_date: i64, winner: MatchSide) -> SeededMatch {
        let radiant_ids = (start_id..start_id + 5).collect::<Vec<_>>();
        let dire_ids = (start_id + 5..start_id + 10).collect::<Vec<_>>();
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO matches (
                     guild_id,winning_team,match_date,win_reward_jc,betting_mode,
                     team1_players,team2_players
                 ) VALUES (?1,?2,?3,?4,'pool',?5,?6)",
                params![
                    GUILD,
                    winner.number(),
                    match_date,
                    WIN_REWARD,
                    json_ids(&radiant_ids),
                    json_ids(&dire_ids)
                ],
            )
            .expect("insert match");
        let match_id = connection.last_insert_rowid();
        for (team, ids) in [
            (MatchSide::Radiant, radiant_ids.as_slice()),
            (MatchSide::Dire, dire_ids.as_slice()),
        ] {
            for &discord_id in ids {
                let won = team == winner;
                let balance = 100
                    + if won {
                        WIN_REWARD
                    } else {
                        PARTICIPATION_REWARD
                    };
                connection
                    .execute(
                        "INSERT INTO players (
                             discord_id,guild_id,discord_username,jopacoin_balance,
                             lowest_balance_ever,wins,losses,glicko_rating,glicko_rd,
                             glicko_volatility,os_mu,os_sigma,initial_mmr
                         ) VALUES (?1,?2,?3,?4,?4,?5,?6,?7,80.0,0.06,?8,8.0,1500)",
                        params![
                            discord_id,
                            GUILD,
                            format!("player{discord_id}"),
                            balance,
                            i64::from(won),
                            i64::from(!won),
                            if won { 1_520.0 } else { 1_480.0 },
                            if won { 36.0 } else { 34.0 }
                        ],
                    )
                    .expect("insert player");
                connection
                    .execute(
                        "INSERT INTO match_participants (
                             match_id,discord_id,team_number,won,side,guild_id,
                             fantasy_points,win_bonus_jc
                         ) VALUES (?1,?2,?3,?4,?5,?6,NULL,?7)",
                        params![
                            match_id,
                            discord_id,
                            team.number(),
                            won,
                            team.label(),
                            GUILD,
                            won.then_some(WIN_REWARD)
                        ],
                    )
                    .expect("insert participant");
                connection
                    .execute(
                        "INSERT INTO rating_history (
                             discord_id,rating,rating_before,rd_before,rd_after,
                             volatility_before,volatility_after,won,match_id,
                             os_mu_before,os_mu_after,os_sigma_before,os_sigma_after,
                             streak_length,streak_multiplier,
                             streak_multiplier_per_game,streak_threshold,
                             base_rating_delta_multiplier,low_priority_gain_multiplier,
                             guild_id
                         ) VALUES (
                             ?1,?2,1500.0,80.0,80.0,0.06,0.06,?3,?4,
                             35.0,?5,8.0,8.0,1,1.0,0.20,3,0.75,1.0,?6
                         )",
                        params![
                            discord_id,
                            if won { 1_520.0 } else { 1_480.0 },
                            won,
                            match_id,
                            if won { 36.0 } else { 34.0 },
                            GUILD
                        ],
                    )
                    .expect("insert rating history");
            }
        }
        seed_pairings(&connection, &radiant_ids, &dire_ids, winner, GUILD);
        SeededMatch {
            match_id,
            radiant_ids,
            dire_ids,
        }
    }

    fn add_match_for_teams(
        &self,
        teams: &SeededMatch,
        match_date: i64,
        winner: MatchSide,
    ) -> SeededMatch {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO matches (
                     guild_id,winning_team,match_date,win_reward_jc,betting_mode,
                     team1_players,team2_players
                 ) VALUES (?1,?2,?3,?4,'pool',?5,?6)",
                params![
                    GUILD,
                    winner.number(),
                    match_date,
                    WIN_REWARD,
                    json_ids(&teams.radiant_ids),
                    json_ids(&teams.dire_ids)
                ],
            )
            .expect("insert replayable match");
        let match_id = connection.last_insert_rowid();
        for (team, ids) in [
            (MatchSide::Radiant, teams.radiant_ids.as_slice()),
            (MatchSide::Dire, teams.dire_ids.as_slice()),
        ] {
            for &discord_id in ids {
                let won = team == winner;
                connection
                    .execute(
                        "UPDATE players SET wins=wins+?1,losses=losses+?2
                         WHERE discord_id=?3 AND guild_id=?4",
                        params![i64::from(won), i64::from(!won), discord_id, GUILD],
                    )
                    .expect("advance replay player counters");
                connection
                    .execute(
                        "INSERT INTO match_participants (
                             match_id,discord_id,team_number,won,side,guild_id,
                             fantasy_points,win_bonus_jc
                         ) VALUES (?1,?2,?3,?4,?5,?6,NULL,?7)",
                        params![
                            match_id,
                            discord_id,
                            team.number(),
                            won,
                            team.label(),
                            GUILD,
                            won.then_some(WIN_REWARD)
                        ],
                    )
                    .expect("insert replay participant");
                connection
                    .execute(
                        "INSERT INTO rating_history (
                             discord_id,rating,rating_before,rd_before,rd_after,
                             volatility_before,volatility_after,won,match_id,
                             os_mu_before,os_mu_after,os_sigma_before,os_sigma_after,
                             streak_length,streak_multiplier,
                             streak_multiplier_per_game,streak_threshold,
                             base_rating_delta_multiplier,low_priority_gain_multiplier,
                             guild_id
                         ) VALUES (
                             ?1,1500.0,1500.0,80.0,80.0,0.06,0.06,?2,?3,
                             35.0,35.0,8.0,8.0,1,1.0,0.20,3,0.75,1.0,?4
                         )",
                        params![discord_id, won, match_id, GUILD],
                    )
                    .expect("insert replay rating history");
            }
        }
        SeededMatch {
            match_id,
            radiant_ids: teams.radiant_ids.clone(),
            dire_ids: teams.dire_ids.clone(),
        }
    }

    fn request_openskill_replay(&self, reason: &str) {
        self.connection()
            .execute(
                "INSERT INTO openskill_replay_jobs(guild_id,reason,requested_at)
                 VALUES (?1,?2,CURRENT_TIMESTAMP)
                 ON CONFLICT(guild_id) DO UPDATE SET reason=excluded.reason,
                     requested_at=excluded.requested_at,last_error=NULL",
                params![GUILD, reason],
            )
            .expect("request replay");
    }

    fn claim(&self, seeded: &SeededMatch, side: MatchSide, owner: &str) -> CorrectionClaim {
        self.repository
            .claim_match_correction(seeded.match_id, Some(GUILD), side, owner, NOW, 300)
            .expect("claim correction")
    }

    fn apply_core(
        &self,
        seeded: &SeededMatch,
        old: MatchSide,
        new: MatchSide,
        owner: &str,
        updates: &[RatingCorrection],
    ) -> Option<i64> {
        self.repository
            .apply_core_correction(&CoreCorrectionRequest {
                match_id: seeded.match_id,
                guild_id: Some(GUILD),
                old_winning_team: old,
                new_winning_team: new,
                corrected_by: Some(ADMIN),
                owner_token: owner,
                rating_updates: updates,
                openskill_algorithm_version: 4,
                openskill_algorithm_fingerprint: "test-v4",
            })
            .expect("apply core correction")
    }

    fn balance(&self, discord_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, GUILD],
                |row| row.get(0),
            )
            .expect("player balance")
    }

    fn counters(&self, discord_id: i64) -> (i64, i64) {
        self.connection()
            .query_row(
                "SELECT wins,losses FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, GUILD],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("player counters")
    }

    fn win_bonus(&self, match_id: i64, discord_id: i64) -> Option<i64> {
        self.connection()
            .query_row(
                "SELECT win_bonus_jc FROM match_participants
                 WHERE match_id=?1 AND discord_id=?2 AND guild_id=?3",
                params![match_id, discord_id, GUILD],
                |row| row.get(0),
            )
            .expect("participant bonus")
    }

    fn bet(
        &self,
        match_id: i64,
        discord_id: i64,
        team: MatchSide,
        amount: i64,
        payout: Option<i64>,
    ) -> i64 {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO bets (
                     discord_id,match_id,team_bet_on,amount,leverage,payout,bet_time
                 ) VALUES (?1,?2,?3,?4,1,?5,?6)",
                params![discord_id, match_id, team.label(), amount, payout, NOW],
            )
            .expect("insert bet");
        connection.last_insert_rowid()
    }

    fn add_spectator(&self, discord_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id,guild_id,discord_username,jopacoin_balance,
                     lowest_balance_ever,wins,losses,glicko_rating,glicko_rd,
                     glicko_volatility,os_mu,os_sigma
                 ) VALUES (?1,?2,?3,?4,?4,0,0,1500.0,80.0,0.06,35.0,8.0)",
                params![discord_id, GUILD, format!("spectator{discord_id}"), balance],
            )
            .expect("insert spectator");
    }

    fn payout(&self, match_id: i64, discord_id: i64) -> Option<i64> {
        self.connection()
            .query_row(
                "SELECT payout FROM bets WHERE match_id=?1 AND discord_id=?2",
                params![match_id, discord_id],
                |row| row.get(0),
            )
            .expect("bet payout")
    }
}

fn json_ids(ids: &[i64]) -> String {
    format!(
        "[{}]",
        ids.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn seed_pairings(
    connection: &Connection,
    radiant_ids: &[i64],
    dire_ids: &[i64],
    winner: MatchSide,
    guild_id: i64,
) {
    for (team, ids) in [
        (MatchSide::Radiant, radiant_ids),
        (MatchSide::Dire, dire_ids),
    ] {
        for (index, &first) in ids.iter().enumerate() {
            for &second in &ids[index + 1..] {
                let (player1, player2) = canonical_pair(first, second);
                connection
                    .execute(
                        "INSERT INTO player_pairings (
                             guild_id,player1_id,player2_id,games_together,
                             wins_together,games_against,player1_wins_against
                         ) VALUES (?1,?2,?3,1,?4,0,0)",
                        params![guild_id, player1, player2, i64::from(team == winner)],
                    )
                    .expect("insert teammate pairing");
            }
        }
    }
    for &radiant in radiant_ids {
        for &dire in dire_ids {
            let (player1, player2) = canonical_pair(radiant, dire);
            let player1_is_radiant = player1 == radiant;
            let player1_won = if player1_is_radiant {
                winner == MatchSide::Radiant
            } else {
                winner == MatchSide::Dire
            };
            connection
                .execute(
                    "INSERT INTO player_pairings (
                         guild_id,player1_id,player2_id,games_together,
                         wins_together,games_against,player1_wins_against
                     ) VALUES (?1,?2,?3,0,0,1,?4)",
                    params![guild_id, player1, player2, i64::from(player1_won)],
                )
                .expect("insert opponent pairing");
        }
    }
}

fn flat_updates(seeded: &SeededMatch, winner: MatchSide) -> Vec<RatingCorrection> {
    seeded
        .radiant_ids
        .iter()
        .chain(&seeded.dire_ids)
        .map(|&discord_id| {
            let won = if seeded.radiant_ids.contains(&discord_id) {
                winner == MatchSide::Radiant
            } else {
                winner == MatchSide::Dire
            };
            RatingCorrection {
                discord_id,
                rating: if won { 1_525.0 } else { 1_475.0 },
                rd: 78.0,
                volatility: 0.06,
                won,
                os_mu: Some(if won { 36.25 } else { 33.75 }),
                os_sigma: Some(7.9),
                fantasy_weight: None,
                streak_length: Some(1),
                streak_multiplier: Some(1.0),
            }
        })
        .collect()
}

fn move_win_bonuses(fixture: &Fixture, seeded: &SeededMatch, new_winner: MatchSide) {
    let (old_ids, new_ids) = if new_winner == MatchSide::Dire {
        (&seeded.radiant_ids, &seeded.dire_ids)
    } else {
        (&seeded.dire_ids, &seeded.radiant_ids)
    };
    for &discord_id in new_ids {
        if fixture
            .repository
            .credit_resolved_win_bonus(seeded.match_id, Some(GUILD), discord_id, WIN_REWARD)
            .expect("award corrected winner")
        {
            fixture
                .repository
                .snapshot_win_bonus(seeded.match_id, Some(GUILD), discord_id, WIN_REWARD)
                .expect("snapshot corrected winner");
        }
    }
    let debits = old_ids
        .iter()
        .filter_map(|&discord_id| {
            fixture
                .win_bonus(seeded.match_id, discord_id)
                .filter(|amount| *amount > 0)
                .map(|amount| (discord_id, amount))
        })
        .collect::<BTreeMap<_, _>>();
    fixture
        .repository
        .reverse_win_bonuses_atomic(seeded.match_id, Some(GUILD), &debits)
        .expect("reverse old winner bonuses");
}

fn set_match_fantasy_points(fixture: &Fixture, seeded: &SeededMatch) {
    let connection = fixture.connection();
    for (index, &discord_id) in seeded
        .radiant_ids
        .iter()
        .chain(&seeded.dire_ids)
        .enumerate()
    {
        connection
            .execute(
                "UPDATE match_participants SET fantasy_points=?1
                 WHERE match_id=?2 AND guild_id=?3 AND discord_id=?4",
                params![5.0 + 3.0 * index as f64, seeded.match_id, GUILD, discord_id],
            )
            .expect("seed distinct fantasy performance");
    }
}

fn approx(left: f64, right: f64) {
    assert!((left - right).abs() < 1.0e-8, "{left} != {right}");
}

#[test]
fn test_recorded_win_reward_uses_historical_fallback_only_for_legacy_matches() {
    assert_eq!(recorded_win_reward_jc(Some(10)), 10);
    assert_eq!(recorded_win_reward_jc(None), 4);
}

#[test]
fn test_correction_context_preserves_recorded_inputs_and_bonus_snapshots() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(18_500);
    let context = fixture
        .repository
        .correction_context(seeded.match_id, Some(GUILD))
        .expect("load correction context");
    assert_eq!(context.match_id, seeded.match_id);
    assert_eq!(context.guild_id, GUILD);
    assert_eq!(context.winning_team, MatchSide::Radiant);
    assert_eq!(context.recorded_win_reward_jc, WIN_REWARD);
    assert!(context.pool_betting_mode);
    assert_eq!(context.participants.len(), 10);
    assert!(context.participants.iter().all(|participant| {
        let radiant = seeded.radiant_ids.contains(&participant.discord_id);
        participant.team
            == if radiant {
                MatchSide::Radiant
            } else {
                MatchSide::Dire
            }
            && participant.win_bonus_jc == radiant.then_some(WIN_REWARD)
    }));

    fixture
        .connection()
        .execute(
            "UPDATE matches SET win_reward_jc=NULL,betting_mode=NULL WHERE match_id=?1",
            [seeded.match_id],
        )
        .expect("make legacy context");
    let legacy = fixture
        .repository
        .correction_context(seeded.match_id, Some(GUILD))
        .expect("load legacy context");
    assert_eq!(legacy.recorded_win_reward_jc, LEGACY_WIN_REWARD_JC);
    assert!(legacy.pool_betting_mode);

    fixture
        .connection()
        .execute(
            "UPDATE matches SET betting_mode='' WHERE match_id=?1",
            [seeded.match_id],
        )
        .expect("store falsey legacy mode");
    let empty = fixture
        .repository
        .correction_context(seeded.match_id, Some(GUILD))
        .expect("load empty legacy mode");
    assert!(empty.pool_betting_mode);

    fixture
        .connection()
        .execute(
            "UPDATE matches SET betting_mode='unknown' WHERE match_id=?1",
            [seeded.match_id],
        )
        .expect("store unknown mode");
    let unknown = fixture
        .repository
        .correction_context(seeded.match_id, Some(GUILD))
        .expect("load unknown mode");
    assert!(!unknown.pool_betting_mode);
}

#[test]
fn test_correction_uses_recorded_low_priority_gain_after_state_clears() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(18_000);
    let target = seeded.dire_ids[0];
    fixture
        .connection()
        .execute(
            "UPDATE rating_history SET low_priority_gain_multiplier=1.10
             WHERE match_id=?1 AND discord_id=?2",
            params![seeded.match_id, target],
        )
        .expect("store recording-time gain");
    fixture
        .connection()
        .execute(
            "INSERT INTO low_priority_state(discord_id,guild_id,active)
             VALUES (?1,?2,0)",
            params![target, GUILD],
        )
        .expect("cleared low-priority state");
    let updates = fixture
        .repository
        .prepare_rating_correction(seeded.match_id, Some(GUILD), MatchSide::Dire)
        .expect("prepare recorded-input correction");
    let target_update = updates
        .iter()
        .find(|update| update.discord_id == target)
        .expect("target update");
    let peer = seeded.dire_ids[1];
    let peer_update = updates
        .iter()
        .find(|update| update.discord_id == peer)
        .expect("peer update");
    approx(
        target_update.rating - 1500.0,
        (peer_update.rating - 1500.0) * 1.10,
    );
    approx(
        target_update.os_mu.unwrap() - 35.0,
        (peer_update.os_mu.unwrap() - 35.0) * 1.10,
    );
    fixture.claim(&seeded, MatchSide::Dire, "low-priority");
    fixture.apply_core(
        &seeded,
        MatchSide::Radiant,
        MatchSide::Dire,
        "low-priority",
        &updates,
    );
    let (active, rating, mu): (i64, f64, f64) = fixture
        .connection()
        .query_row(
            "SELECT lp.active,p.glicko_rating,p.os_mu
             FROM low_priority_state lp
             JOIN players p ON p.discord_id=lp.discord_id AND p.guild_id=lp.guild_id
             WHERE lp.discord_id=?1 AND lp.guild_id=?2",
            params![target, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(active, 0);
    approx(rating, target_update.rating);
    approx(mu, target_update.os_mu.unwrap());
}

#[test]
fn test_match_repository_requires_exact_once_bonus_capabilities() {
    fn construction_requires_contract<T: MatchCorrectionPersistence>() {}
    construction_requires_contract::<MatchCorrectionRepository>();
}

#[test]
fn test_bet_repository_requires_correction_capabilities() {
    fn construction_requires_contract<T: BetCorrectionPersistence>() {}
    construction_requires_contract::<MatchCorrectionRepository>();
}

#[test]
fn test_match_correction_claim_has_one_concurrent_owner() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(17_000);
    let match_id = seeded.match_id;
    let path = fixture.file.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(2));
    let handles = ["owner-a", "owner-b"].map(|owner| {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            MatchCorrectionRepository::new(path).claim_match_correction(
                match_id,
                Some(GUILD),
                MatchSide::Dire,
                owner,
                NOW,
                300,
            )
        })
    });
    let results = handles.map(|handle| handle.join().expect("claim thread"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(MatchCorrectionError::CorrectionInProgress(_))))
            .count(),
        1
    );
}

#[test]
fn test_concurrent_match_corrections_apply_transition_once() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(17_100);
    let path = fixture.file.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(2));
    let handles = [("owner-a", 88_001), ("owner-b", 88_002)].map(|(owner, admin)| {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        let seeded = seeded.clone();
        thread::spawn(move || {
            barrier.wait();
            let repository = MatchCorrectionRepository::new(path);
            let claim = repository.claim_match_correction(
                seeded.match_id,
                Some(GUILD),
                MatchSide::Dire,
                owner,
                NOW,
                300,
            )?;
            let updates = flat_updates(&seeded, MatchSide::Dire);
            repository.apply_core_correction(&CoreCorrectionRequest {
                match_id: seeded.match_id,
                guild_id: Some(GUILD),
                old_winning_team: MatchSide::Radiant,
                new_winning_team: MatchSide::Dire,
                corrected_by: Some(admin),
                owner_token: owner,
                rating_updates: &updates,
                openskill_algorithm_version: 4,
                openskill_algorithm_fingerprint: "test-v4",
            })?;
            Ok::<_, MatchCorrectionError>(claim)
        })
    });
    let results = handles.map(|handle| handle.join().expect("correction thread"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        fixture
            .repository
            .current_winner(seeded.match_id, Some(GUILD))
            .unwrap(),
        MatchSide::Dire
    );
    assert_eq!(
        fixture
            .repository
            .corrections(seeded.match_id)
            .unwrap()
            .len(),
        1
    );
    for discord_id in seeded.radiant_ids {
        assert_eq!(fixture.counters(discord_id), (0, 1));
    }
    for discord_id in seeded.dire_ids {
        assert_eq!(fixture.counters(discord_id), (1, 0));
    }
}

#[test]
fn test_correction_lifecycle_updates_counters_ratings_and_audit() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(1_000);
    assert!(matches!(
        fixture.repository.claim_match_correction(
            99_999,
            Some(GUILD),
            MatchSide::Dire,
            "missing",
            NOW,
            300
        ),
        Err(MatchCorrectionError::MatchNotFound(99_999))
    ));
    let unchanged = fixture
        .repository
        .claim_match_correction(
            seeded.match_id,
            Some(GUILD),
            MatchSide::Radiant,
            "unchanged",
            NOW,
            300,
        )
        .unwrap();
    assert_eq!(unchanged.state, ClaimState::Already);

    fixture.claim(&seeded, MatchSide::Dire, "first");
    let first_updates = flat_updates(&seeded, MatchSide::Dire);
    let first_id = fixture
        .apply_core(
            &seeded,
            MatchSide::Radiant,
            MatchSide::Dire,
            "first",
            &first_updates,
        )
        .expect("audit id");
    assert!(first_id > 0);
    assert_eq!(
        fixture
            .repository
            .corrections(seeded.match_id)
            .unwrap()
            .len(),
        1
    );
    assert!(
        fixture
            .repository
            .complete_match_correction_claim(seeded.match_id, "first")
            .unwrap()
    );

    fixture.claim(&seeded, MatchSide::Radiant, "second");
    let second_updates = flat_updates(&seeded, MatchSide::Radiant);
    fixture.apply_core(
        &seeded,
        MatchSide::Dire,
        MatchSide::Radiant,
        "second",
        &second_updates,
    );
    assert_eq!(
        fixture
            .repository
            .corrections(seeded.match_id)
            .unwrap()
            .len(),
        2
    );
    for discord_id in seeded.radiant_ids {
        assert_eq!(fixture.counters(discord_id), (1, 0));
    }
    for discord_id in seeded.dire_ids {
        assert_eq!(fixture.counters(discord_id), (0, 1));
    }
}

#[test]
fn test_flip_winning_team_swaps_counters_and_pairings() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(2_000);
    let radiant_pair = canonical_pair(seeded.radiant_ids[0], seeded.radiant_ids[1]);
    let dire_pair = canonical_pair(seeded.dire_ids[0], seeded.dire_ids[1]);
    let opponent_pair = canonical_pair(seeded.radiant_ids[0], seeded.dire_ids[0]);
    fixture.claim(&seeded, MatchSide::Dire, "atomic-correction-1");
    let updates = flat_updates(&seeded, MatchSide::Dire);

    let correction_id = fixture
        .apply_core(
            &seeded,
            MatchSide::Radiant,
            MatchSide::Dire,
            "atomic-correction-1",
            &updates,
        )
        .expect("correction audit id");

    assert!(correction_id > 0);
    assert_eq!(
        fixture
            .repository
            .current_winner(seeded.match_id, Some(GUILD))
            .unwrap(),
        MatchSide::Dire
    );
    for discord_id in &seeded.radiant_ids {
        assert_eq!(fixture.counters(*discord_id), (0, 1));
    }
    for discord_id in &seeded.dire_ids {
        assert_eq!(fixture.counters(*discord_id), (1, 0));
    }
    let connection = fixture.connection();
    let pairing_wins = |pair: (i64, i64), column: &str| {
        connection
            .query_row(
                &format!(
                    "SELECT {column} FROM player_pairings
                     WHERE guild_id=?1 AND player1_id=?2 AND player2_id=?3"
                ),
                params![GUILD, pair.0, pair.1],
                |row| row.get::<_, i64>(0),
            )
            .expect("pairing win count")
    };
    assert_eq!(pairing_wins(radiant_pair, "wins_together"), 0);
    assert_eq!(pairing_wins(dire_pair, "wins_together"), 1);
    assert_eq!(pairing_wins(opponent_pair, "player1_wins_against"), 0);
}

#[test]
fn test_correction_without_corrected_by_returns_none() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(2_100);
    fixture.claim(&seeded, MatchSide::Dire, "atomic-correction-2");
    let correction_id = fixture
        .repository
        .apply_core_correction(&CoreCorrectionRequest {
            match_id: seeded.match_id,
            guild_id: Some(GUILD),
            old_winning_team: MatchSide::Radiant,
            new_winning_team: MatchSide::Dire,
            corrected_by: None,
            owner_token: "atomic-correction-2",
            rating_updates: &[],
            openskill_algorithm_version: 4,
            openskill_algorithm_fingerprint: "test-v4",
        })
        .expect("result-only correction");
    assert_eq!(correction_id, None);
    assert_eq!(
        fixture
            .repository
            .current_winner(seeded.match_id, Some(GUILD))
            .unwrap(),
        MatchSide::Dire
    );
}

#[test]
fn test_correction_reverses_bet_payouts() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(3_000);
    let radiant_bettor = seeded.radiant_ids[0];
    let spectator = 3_999;
    fixture.add_spectator(spectator, 100);
    fixture.bet(
        seeded.match_id,
        radiant_bettor,
        MatchSide::Radiant,
        20,
        Some(70),
    );
    fixture.bet(seeded.match_id, spectator, MatchSide::Dire, 50, None);
    let radiant_before = fixture.balance(radiant_bettor);
    let spectator_before = fixture.balance(spectator);
    let result = fixture
        .repository
        .settle_bet_correction_atomic(
            seeded.match_id,
            Some(GUILD),
            MatchSide::Dire,
            BetCorrectionOptions::pool(0.0, &BTreeSet::new()),
        )
        .unwrap();
    assert_eq!(result.balance_changes[&spectator], 70);
    assert_eq!(result.balance_changes[&radiant_bettor], -70);
    assert_eq!(fixture.balance(spectator), spectator_before + 70);
    assert_eq!(fixture.balance(radiant_bettor), radiant_before - 70);
    assert_eq!(fixture.payout(seeded.match_id, spectator), Some(70));
    assert_eq!(fixture.payout(seeded.match_id, radiant_bettor), None);

    // House mode preserves Python's durable per-row allocation: plain odds
    // on each row and all carried surplus on that user's final bet.
    let house = Fixture::new();
    let match_ = house.seed_match(3_100);
    let old_bettor = 3_198;
    let new_bettor = 3_199;
    house.add_spectator(old_bettor, 100);
    house.add_spectator(new_bettor, 100);
    house.bet(
        match_.match_id,
        old_bettor,
        MatchSide::Radiant,
        40,
        Some(100),
    );
    house.bet(match_.match_id, new_bettor, MatchSide::Dire, 10, None);
    house.bet(match_.match_id, new_bettor, MatchSide::Dire, 30, None);
    house
        .repository
        .settle_bet_correction_atomic(
            match_.match_id,
            Some(GUILD),
            MatchSide::Dire,
            BetCorrectionOptions {
                pool_mode: false,
                house_payout_multiplier: 1.0,
                vanity_tax_rate: 0.0,
                vanity_taxable_ids: &BTreeSet::new(),
            },
        )
        .unwrap();
    let house_rows = house
        .repository
        .get_settled_bets_for_match(match_.match_id)
        .unwrap()
        .into_iter()
        .filter(|bet| bet.discord_id == new_bettor)
        .map(|bet| bet.payout.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(house_rows, vec![20, 80]);
}

#[test]
fn test_correction_bet_settlement_is_all_or_nothing() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(6_000);
    let radiant_bettor = seeded.radiant_ids[0];
    let spectator = 6_999;
    fixture.add_spectator(spectator, 100);
    fixture.bet(
        seeded.match_id,
        radiant_bettor,
        MatchSide::Radiant,
        20,
        Some(70),
    );
    fixture.bet(seeded.match_id, spectator, MatchSide::Dire, 50, None);
    let balances_before = (fixture.balance(radiant_bettor), fixture.balance(spectator));
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_bet_correction_balance
             BEFORE UPDATE OF jopacoin_balance ON players
             WHEN (SELECT source FROM economy_ledger_context WHERE id=1)='bet_correction'
             BEGIN SELECT RAISE(ABORT,'injected balance-write failure'); END;",
        )
        .unwrap();
    assert!(
        fixture
            .repository
            .settle_bet_correction_atomic(
                seeded.match_id,
                Some(GUILD),
                MatchSide::Dire,
                BetCorrectionOptions::pool(0.0, &BTreeSet::new())
            )
            .is_err()
    );
    assert_eq!(
        (fixture.balance(radiant_bettor), fixture.balance(spectator)),
        balances_before
    );
    assert_eq!(fixture.payout(seeded.match_id, radiant_bettor), Some(70));
    assert_eq!(fixture.payout(seeded.match_id, spectator), None);
    let context_count: i64 = fixture
        .connection()
        .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(context_count, 0);
}

#[test]
fn test_retry_finishes_bets_before_recovering_openskill() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(6_100);
    let radiant_bettor = seeded.radiant_ids[0];
    let spectator = 6_199;
    fixture.add_spectator(spectator, 100);
    fixture.bet(
        seeded.match_id,
        radiant_bettor,
        MatchSide::Radiant,
        20,
        Some(70),
    );
    fixture.bet(seeded.match_id, spectator, MatchSide::Dire, 50, None);
    fixture.claim(&seeded, MatchSide::Dire, "recovery-1");
    fixture.apply_core(
        &seeded,
        MatchSide::Radiant,
        MatchSide::Dire,
        "recovery-1",
        &flat_updates(&seeded, MatchSide::Dire),
    );
    assert_eq!(
        fixture
            .repository
            .pending_openskill_replay(Some(GUILD))
            .unwrap()
            .as_deref(),
        Some(format!("match_correction:{}", seeded.match_id).as_str())
    );
    fixture
        .repository
        .release_match_correction_claim(seeded.match_id, "recovery-1")
        .unwrap();
    let resumed = fixture
        .repository
        .claim_match_correction(
            seeded.match_id,
            Some(GUILD),
            MatchSide::Dire,
            "recovery-2",
            NOW + 1,
            300,
        )
        .unwrap();
    assert_eq!(resumed.state, ClaimState::CoreApplied);
    assert!(
        !fixture
            .repository
            .bet_correction_complete(seeded.match_id, MatchSide::Dire)
            .unwrap()
    );
    fixture
        .repository
        .settle_bet_correction_atomic(
            seeded.match_id,
            Some(GUILD),
            MatchSide::Dire,
            BetCorrectionOptions::pool(0.0, &BTreeSet::new()),
        )
        .unwrap();
    let balances_after_settlement = (fixture.balance(radiant_bettor), fixture.balance(spectator));
    assert!(
        fixture
            .repository
            .bet_correction_complete(seeded.match_id, MatchSide::Dire)
            .unwrap()
    );
    let duplicate = fixture
        .repository
        .settle_bet_correction_atomic(
            seeded.match_id,
            Some(GUILD),
            MatchSide::Dire,
            BetCorrectionOptions::pool(0.0, &BTreeSet::new()),
        )
        .unwrap();
    assert!(duplicate.balance_changes.is_empty());
    assert_eq!(
        (fixture.balance(radiant_bettor), fixture.balance(spectator)),
        balances_after_settlement
    );
    assert!(
        fixture
            .repository
            .complete_openskill_replay(Some(GUILD))
            .unwrap()
    );
    assert!(
        fixture
            .repository
            .complete_match_correction_claim(seeded.match_id, "recovery-2")
            .unwrap()
    );
    assert_eq!(
        (fixture.balance(radiant_bettor), fixture.balance(spectator)),
        balances_after_settlement
    );
    assert_eq!(
        fixture
            .repository
            .corrections(seeded.match_id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn test_correction_refunds_old_vanity_tax_and_taxes_new_winner() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(3_200);
    let radiant_bettor = 3_298;
    let dire_bettor = 3_299;
    fixture.add_spectator(radiant_bettor, 290);
    fixture.add_spectator(dire_bettor, 100);
    fixture.bet(
        seeded.match_id,
        radiant_bettor,
        MatchSide::Radiant,
        100,
        Some(200),
    );
    fixture.bet(seeded.match_id, dire_bettor, MatchSide::Dire, 100, None);
    fixture
        .connection()
        .execute(
            "INSERT INTO bet_settlement_taxes(match_id,guild_id,discord_id,vanity_tax)
             VALUES (?1,?2,?3,10)",
            params![seeded.match_id, GUILD, radiant_bettor],
        )
        .unwrap();
    let taxable = BTreeSet::from([radiant_bettor, dire_bettor]);
    fixture
        .repository
        .settle_bet_correction_atomic(
            seeded.match_id,
            Some(GUILD),
            MatchSide::Dire,
            BetCorrectionOptions::pool(0.10, &taxable),
        )
        .unwrap();
    assert_eq!(fixture.balance(radiant_bettor), 100);
    assert_eq!(fixture.balance(dire_bettor), 290);
    let taxes: Vec<(i64, i64)> = fixture
        .connection()
        .prepare(
            "SELECT discord_id,vanity_tax FROM bet_settlement_taxes
             WHERE match_id=?1 ORDER BY discord_id",
        )
        .unwrap()
        .query_map([seeded.match_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(taxes, vec![(dire_bettor, 10)]);
}

#[test]
fn test_correction_updates_pairings() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(4_000);
    let radiant_pair = canonical_pair(seeded.radiant_ids[0], seeded.radiant_ids[1]);
    let dire_pair = canonical_pair(seeded.dire_ids[0], seeded.dire_ids[1]);
    fixture.claim(&seeded, MatchSide::Dire, "pairings");
    fixture.apply_core(
        &seeded,
        MatchSide::Radiant,
        MatchSide::Dire,
        "pairings",
        &flat_updates(&seeded, MatchSide::Dire),
    );
    let wins = |pair: (i64, i64)| {
        fixture
            .connection()
            .query_row(
                "SELECT games_together,wins_together FROM player_pairings
                 WHERE guild_id=?1 AND player1_id=?2 AND player2_id=?3",
                params![GUILD, pair.0, pair.1],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap()
    };
    assert_eq!(wins(radiant_pair), (1, 0));
    assert_eq!(wins(dire_pair), (1, 1));
}

#[test]
fn test_correction_moves_win_bonus_exactly() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(9_000);
    move_win_bonuses(&fixture, &seeded, MatchSide::Dire);
    fixture.claim(&seeded, MatchSide::Dire, "bonus-dire");
    fixture.apply_core(
        &seeded,
        MatchSide::Radiant,
        MatchSide::Dire,
        "bonus-dire",
        &flat_updates(&seeded, MatchSide::Dire),
    );
    fixture
        .repository
        .complete_match_correction_claim(seeded.match_id, "bonus-dire")
        .unwrap();
    for &discord_id in &seeded.radiant_ids {
        assert_eq!(fixture.balance(discord_id), 100);
    }
    for &discord_id in &seeded.dire_ids {
        assert_eq!(fixture.balance(discord_id), 115);
    }

    move_win_bonuses(&fixture, &seeded, MatchSide::Radiant);
    fixture.claim(&seeded, MatchSide::Radiant, "bonus-radiant");
    fixture.apply_core(
        &seeded,
        MatchSide::Dire,
        MatchSide::Radiant,
        "bonus-radiant",
        &flat_updates(&seeded, MatchSide::Radiant),
    );
    for &discord_id in &seeded.radiant_ids {
        assert_eq!(fixture.balance(discord_id), 110);
    }
    for &discord_id in &seeded.dire_ids {
        assert_eq!(fixture.balance(discord_id), 105);
    }
}

#[test]
fn test_correction_applies_streak_multipliers() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(11_000);
    let connection = fixture.connection();
    for &discord_id in &seeded.dire_ids {
        for _ in 0..3 {
            connection
                .execute(
                    "INSERT INTO rating_history(discord_id,rating,won,match_id,guild_id)
                     VALUES (?1,1500.0,1,0,?2)",
                    params![discord_id, GUILD],
                )
                .unwrap();
        }
    }
    drop(connection);
    let updates = fixture
        .repository
        .prepare_rating_correction(seeded.match_id, Some(GUILD), MatchSide::Dire)
        .unwrap();
    let system = CamaRatingSystem::default();
    let to_team = |ids: &[i64]| {
        ids.iter()
            .map(|&discord_id| {
                TeamPlayer::new(
                    system.create_player_from_rating(1500.0, 80.0, 0.06),
                    discord_id,
                )
            })
            .collect::<Vec<_>>()
    };
    let expected = system
        .update_ratings_after_match_with_options(
            &to_team(&seeded.radiant_ids),
            &to_team(&seeded.dire_ids),
            2,
            &MatchUpdateOptions {
                streak_multipliers: seeded
                    .radiant_ids
                    .iter()
                    .map(|&discord_id| (discord_id, 1.0))
                    .chain(seeded.dire_ids.iter().map(|&discord_id| (discord_id, 1.40)))
                    .collect(),
                base_rating_delta_multiplier: Some(0.75),
                gain_multipliers: HashMap::new(),
            },
        )
        .unwrap();
    let expected = expected
        .team1
        .into_iter()
        .chain(expected.team2)
        .map(|update| (update.id, update.rating))
        .collect::<BTreeMap<_, _>>();
    for update in &updates {
        approx(update.rating, expected[&update.discord_id]);
    }
    fixture.claim(&seeded, MatchSide::Dire, "streak");
    fixture.apply_core(
        &seeded,
        MatchSide::Radiant,
        MatchSide::Dire,
        "streak",
        &updates,
    );
    for &discord_id in &seeded.dire_ids {
        let stored: (i64, f64, f64) = fixture
            .connection()
            .query_row(
                "SELECT rh.streak_length,rh.streak_multiplier,p.glicko_rating
                 FROM rating_history rh
                 JOIN players p ON p.discord_id=rh.discord_id AND p.guild_id=rh.guild_id
                 WHERE rh.match_id=?1 AND rh.discord_id=?2",
                params![seeded.match_id, discord_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored.0, 4);
        approx(stored.1, 1.40);
        approx(stored.2, expected[&discord_id]);
    }
}

#[test]
fn test_correction_preserves_recording_time_streak_rate_after_config_change() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(11_500);
    let connection = fixture.connection();
    for &discord_id in &seeded.dire_ids {
        for _ in 0..3 {
            connection
                .execute(
                    "INSERT INTO rating_history(discord_id,rating,won,match_id,guild_id)
                     VALUES (?1,1500.0,1,0,?2)",
                    params![discord_id, GUILD],
                )
                .unwrap();
        }
    }
    drop(connection);
    let live_config = RatingConfig {
        streak_multiplier_per_game: 0.25,
        streak_threshold: 5,
        ..RatingConfig::default()
    };
    let live = CamaRatingSystem::with_config(350.0, 0.06, live_config);
    let wrong_live = live.calculate_streak_multiplier(&[true, true, true], true, None, None);
    approx(wrong_live.multiplier, 1.0);
    let updates = fixture
        .repository
        .prepare_rating_correction(seeded.match_id, Some(GUILD), MatchSide::Dire)
        .unwrap();
    assert!(seeded.dire_ids.iter().all(|discord_id| {
        updates
            .iter()
            .find(|update| update.discord_id == *discord_id)
            .is_some_and(|update| {
                update.streak_length == Some(4) && update.streak_multiplier == Some(1.40)
            })
    }));
    fixture.claim(&seeded, MatchSide::Dire, "recorded-streak");
    fixture.apply_core(
        &seeded,
        MatchSide::Radiant,
        MatchSide::Dire,
        "recorded-streak",
        &updates,
    );
    for discord_id in seeded.dire_ids {
        let stored: (i64, f64) = fixture
            .connection()
            .query_row(
                "SELECT streak_length,streak_multiplier FROM rating_history
                 WHERE match_id=?1 AND discord_id=?2",
                params![seeded.match_id, discord_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored.0, 4);
        approx(stored.1, 1.40);
    }
}

#[test]
fn test_correction_preserves_recording_time_base_delta_multiplier() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(11_700);
    fixture
        .connection()
        .execute(
            "UPDATE rating_history SET base_rating_delta_multiplier=0.80
             WHERE match_id=?1",
            [seeded.match_id],
        )
        .unwrap();
    let updates = fixture
        .repository
        .prepare_rating_correction(seeded.match_id, Some(GUILD), MatchSide::Dire)
        .unwrap();
    let system = CamaRatingSystem::default();
    let to_team = |ids: &[i64]| {
        ids.iter()
            .map(|&discord_id| {
                TeamPlayer::new(
                    system.create_player_from_rating(1500.0, 80.0, 0.06),
                    discord_id,
                )
            })
            .collect::<Vec<_>>()
    };
    let team1 = to_team(&seeded.radiant_ids);
    let team2 = to_team(&seeded.dire_ids);
    let recorded_options = MatchUpdateOptions {
        base_rating_delta_multiplier: Some(0.80),
        ..MatchUpdateOptions::default()
    };
    let current_options = MatchUpdateOptions {
        base_rating_delta_multiplier: Some(0.98),
        ..MatchUpdateOptions::default()
    };
    let expected = system
        .update_ratings_after_match_with_options(&team1, &team2, 2, &recorded_options)
        .unwrap();
    let expected = expected
        .team1
        .into_iter()
        .chain(expected.team2)
        .map(|update| (update.id, update.rating))
        .collect::<BTreeMap<_, _>>();
    let wrong = system
        .update_ratings_after_match_with_options(&team1, &team2, 2, &current_options)
        .unwrap()
        .team1[0]
        .rating;
    assert!((expected[&seeded.radiant_ids[0]] - wrong).abs() > 1.0e-6);
    for update in &updates {
        approx(update.rating, expected[&update.discord_id]);
    }
    fixture.claim(&seeded, MatchSide::Dire, "recorded-base");
    fixture.apply_core(
        &seeded,
        MatchSide::Radiant,
        MatchSide::Dire,
        "recorded-base",
        &updates,
    );
    for update in updates {
        let stored: (f64, f64, f64) = fixture
            .connection()
            .query_row(
                "SELECT rh.base_rating_delta_multiplier,rh.rating,p.glicko_rating
                 FROM rating_history rh
                 JOIN players p ON p.discord_id=rh.discord_id AND p.guild_id=rh.guild_id
                 WHERE rh.match_id=?1 AND rh.discord_id=?2",
                params![seeded.match_id, update.discord_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        approx(stored.0, 0.80);
        approx(stored.1, expected[&update.discord_id]);
        approx(stored.2, expected[&update.discord_id]);
    }
}

#[test]
fn test_correction_replay_uses_recorded_streak_rate_for_openskill() {
    let fixture = Fixture::new();
    let first = fixture.seed_match_at(11_600, NOW - 3, MatchSide::Dire);
    fixture.add_match_for_teams(&first, NOW - 2, MatchSide::Dire);
    fixture.add_match_for_teams(&first, NOW - 1, MatchSide::Dire);
    let seeded = fixture.add_match_for_teams(&first, NOW, MatchSide::Radiant);
    let target = seeded.dire_ids[0];

    // Establish the original result through the same chronological worker
    // that correction recovery invokes. The target enters match four on a
    // three-win streak, so its pre-match snapshot is a real replay product.
    fixture.request_openskill_replay("initial-history");
    let initial = fixture
        .repository
        .replay_openskill_atomic(Some(GUILD))
        .unwrap();
    assert!(initial.errors.is_empty());
    assert_eq!(initial.matches_processed, 4);

    let updates = fixture
        .repository
        .prepare_rating_correction(seeded.match_id, Some(GUILD), MatchSide::Dire)
        .unwrap();
    let target_update = updates
        .iter()
        .find(|update| update.discord_id == target)
        .unwrap();
    let expected = target_update.os_mu.unwrap();
    assert_eq!(target_update.streak_length, Some(4));
    assert_eq!(target_update.streak_multiplier, Some(1.40));

    let system = CamaOpenSkillSystem::default();
    let snapshots = fixture
        .repository
        .rating_snapshots(seeded.match_id, Some(GUILD))
        .unwrap()
        .into_iter()
        .map(|snapshot| (snapshot.discord_id, snapshot))
        .collect::<BTreeMap<_, _>>();
    let radiant = seeded
        .radiant_ids
        .iter()
        .map(|&id| {
            OpenSkillPlayer::new(
                id as u64,
                snapshots[&id].os_mu_before,
                snapshots[&id].os_sigma_before,
            )
        })
        .collect::<Vec<_>>();
    let dire = seeded
        .dire_ids
        .iter()
        .map(|&id| {
            OpenSkillPlayer::new(
                id as u64,
                snapshots[&id].os_mu_before,
                snapshots[&id].os_sigma_before,
            )
        })
        .collect::<Vec<_>>();
    let current_multiplier = 1.0 + 0.25 * 2.0;
    let current_streaks = BTreeMap::from([(target as u64, current_multiplier)]);
    let wrong = system
        .update_ratings_equal_weight(
            &radiant,
            &dire,
            WinningTeam::Team2,
            &current_streaks,
            &BTreeMap::new(),
        )
        .unwrap()[&(target as u64)]
        .mu;
    assert!((expected - wrong).abs() > 1.0e-8);
    fixture.claim(&seeded, MatchSide::Dire, "openskill");
    fixture.apply_core(
        &seeded,
        MatchSide::Radiant,
        MatchSide::Dire,
        "openskill",
        &updates,
    );
    assert!(
        fixture
            .repository
            .pending_openskill_replay(Some(GUILD))
            .unwrap()
            .is_some()
    );
    let replayed = fixture
        .repository
        .replay_openskill_atomic(Some(GUILD))
        .unwrap();
    assert!(replayed.errors.is_empty());
    assert_eq!(replayed.matches_processed, 4);
    assert_eq!(replayed.matches_equal_weight, 4);
    assert!(
        fixture
            .repository
            .pending_openskill_replay(Some(GUILD))
            .unwrap()
            .is_none()
    );
    let stored: (f64, i64, f64, i64, String) = fixture
        .connection()
        .query_row(
            "SELECT os_mu_after,streak_length,streak_multiplier,
                    os_algorithm_version,os_algorithm_fingerprint
             FROM rating_history WHERE match_id=?1 AND discord_id=?2",
            params![seeded.match_id, target],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    approx(stored.0, expected);
    assert_eq!(stored.1, 4);
    approx(stored.2, 1.40);
    assert_eq!(stored.3, OPENSKILL_REPLAY_ALGORITHM_VERSION);
    assert_eq!(stored.4, system.algorithm_fingerprint());
    let live_mu: f64 = fixture
        .connection()
        .query_row(
            "SELECT os_mu FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![target, GUILD],
            |row| row.get(0),
        )
        .unwrap();
    approx(live_mu, expected);
}

fn assert_replay_reconstructs_streaks_with_recorded_rate_and_bounded_history(with_fantasy: bool) {
    let fixture = Fixture::new();
    let first = fixture.seed_match_at(12_100, NOW, MatchSide::Radiant);
    let mut matches = vec![first.clone()];
    for offset in 1..22 {
        matches.push(fixture.add_match_for_teams(
            &first,
            NOW + i64::from(offset),
            MatchSide::Radiant,
        ));
    }

    for (offset, replay_match) in matches.iter().enumerate() {
        let rate = if offset == 3 { 0.30 } else { 0.10 };
        let connection = fixture.connection();
        connection
            .execute(
                "UPDATE rating_history
                 SET streak_multiplier_per_game=?1,streak_threshold=3
                 WHERE match_id=?2 AND guild_id=?3",
                params![rate, replay_match.match_id, GUILD],
            )
            .expect("record match-specific streak curve");
        if with_fantasy {
            connection
                .execute(
                    "UPDATE match_participants
                     SET fantasy_points=10.0+(discord_id-?1)
                     WHERE match_id=?2 AND guild_id=?3",
                    params![first.radiant_ids[0], replay_match.match_id, GUILD],
                )
                .expect("seed complete fantasy performance");
        }
    }

    fixture.request_openskill_replay("streak-history");
    let summary = fixture
        .repository
        .replay_openskill_atomic(Some(GUILD))
        .expect("replay bounded streak history");
    assert!(summary.errors.is_empty());
    assert_eq!(summary.matches_processed, 22);
    assert_eq!(summary.matches_with_fantasy, usize::from(with_fantasy) * 22);
    assert_eq!(
        summary.matches_equal_weight,
        usize::from(!with_fantasy) * 22
    );

    let rating_system = CamaRatingSystem::default();
    let all_ids = first
        .radiant_ids
        .iter()
        .chain(&first.dire_ids)
        .copied()
        .collect::<Vec<_>>();
    let winning_target = first.radiant_ids[0];
    let losing_target = first.dire_ids[0];
    let mut recent_outcomes = BTreeMap::new();
    let mut calls = Vec::new();
    for offset in 0..22 {
        let rate = if offset == 3 { 0.30 } else { 0.10 };
        calls.push(
            replay_streak_multipliers(
                &rating_system,
                &all_ids,
                &first.radiant_ids,
                MatchSide::Radiant,
                &recent_outcomes,
                rate,
                3,
            )
            .expect("valid replay IDs"),
        );
        for &discord_id in &all_ids {
            record_replay_outcome(
                &mut recent_outcomes,
                discord_id,
                first.radiant_ids.contains(&discord_id),
            );
        }
    }
    let multiplier = |match_index: usize, discord_id: i64| {
        calls[match_index][&u64::try_from(discord_id).unwrap()]
    };
    approx(multiplier(0, winning_target), 1.0);
    approx(multiplier(1, winning_target), 1.0);
    approx(multiplier(2, winning_target), 1.10);
    approx(multiplier(3, winning_target), 1.60);
    approx(multiplier(3, losing_target), 1.60);
    approx(multiplier(20, winning_target), 2.90);
    approx(multiplier(21, winning_target), 2.90);
    assert_eq!(recent_outcomes[&winning_target].len(), RECENT_OUTCOME_LIMIT);
}

#[test]
fn test_replay_reconstructs_streaks_with_recorded_rate_and_bounded_history_false() {
    assert_replay_reconstructs_streaks_with_recorded_rate_and_bounded_history(false);
}

#[test]
fn test_replay_reconstructs_streaks_with_recorded_rate_and_bounded_history_true() {
    assert_replay_reconstructs_streaks_with_recorded_rate_and_bounded_history(true);
}

#[test]
fn test_replay_honors_recorded_streak_threshold() {
    let rating_system = CamaRatingSystem::default();
    let radiant_ids = (12_400..12_405).collect::<Vec<_>>();
    let dire_ids = (12_405..12_410).collect::<Vec<_>>();
    let all_ids = radiant_ids
        .iter()
        .chain(&dire_ids)
        .copied()
        .collect::<Vec<_>>();
    let target = radiant_ids[0];
    let mut recent_outcomes = BTreeMap::new();
    let mut multipliers = Vec::new();
    for _ in 0..4 {
        multipliers.push(
            replay_streak_multipliers(
                &rating_system,
                &all_ids,
                &radiant_ids,
                MatchSide::Radiant,
                &recent_outcomes,
                0.30,
                4,
            )
            .expect("valid replay IDs"),
        );
        for &discord_id in &all_ids {
            record_replay_outcome(
                &mut recent_outcomes,
                discord_id,
                radiant_ids.contains(&discord_id),
            );
        }
    }
    approx(multipliers[2][&u64::try_from(target).unwrap()], 1.0);
    approx(multipliers[3][&u64::try_from(target).unwrap()], 1.60);
}

#[test]
fn test_replay_uses_legacy_streak_rate_when_match_rate_is_unavailable() {
    let fixture = Fixture::new();
    let first = fixture.seed_match_at(12_500, NOW, MatchSide::Radiant);
    fixture.add_match_for_teams(&first, NOW + 1, MatchSide::Radiant);
    fixture.add_match_for_teams(&first, NOW + 2, MatchSide::Radiant);
    fixture
        .connection()
        .execute(
            "UPDATE rating_history SET streak_multiplier_per_game=NULL
             WHERE guild_id=?1",
            [GUILD],
        )
        .expect("remove recording-time streak rate");
    fixture.request_openskill_replay("legacy-streak-rate");
    let summary = fixture
        .repository
        .replay_openskill_atomic(Some(GUILD))
        .expect("replay legacy streak curve");
    assert!(summary.errors.is_empty());
    assert_eq!(summary.matches_processed, 3);

    let rating_system = CamaRatingSystem::default();
    let all_ids = first
        .radiant_ids
        .iter()
        .chain(&first.dire_ids)
        .copied()
        .collect::<Vec<_>>();
    let mut recent_outcomes = BTreeMap::new();
    for _ in 0..2 {
        for &discord_id in &all_ids {
            record_replay_outcome(
                &mut recent_outcomes,
                discord_id,
                first.radiant_ids.contains(&discord_id),
            );
        }
    }
    let rate = recorded_streak_rate(RecordedValue::Null);
    let multipliers = replay_streak_multipliers(
        &rating_system,
        &all_ids,
        &first.radiant_ids,
        MatchSide::Radiant,
        &recent_outcomes,
        rate,
        3,
    )
    .expect("valid replay IDs");
    approx(
        multipliers[&u64::try_from(first.radiant_ids[0]).unwrap()],
        1.20,
    );
    assert_ne!(
        rate,
        CamaOpenSkillSystem::default()
            .config()
            .streak_multiplier_per_game
    );
}

#[test]
fn test_enriching_latest_match_updates_players() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match_at(12_600, NOW, MatchSide::Radiant);
    fixture.request_openskill_replay("phase-one");
    let phase_one = fixture
        .repository
        .replay_openskill_atomic(Some(GUILD))
        .expect("establish equal-weight history");
    assert!(phase_one.errors.is_empty());
    assert_eq!(phase_one.matches_equal_weight, 1);
    let ids = seeded
        .radiant_ids
        .iter()
        .chain(&seeded.dire_ids)
        .copied()
        .collect::<Vec<_>>();
    let phase_one_live = ids
        .iter()
        .map(|discord_id| {
            let rating = fixture
                .connection()
                .query_row(
                    "SELECT os_mu,os_sigma FROM players
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![discord_id, GUILD],
                    |row| Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?)),
                )
                .expect("load phase-one rating");
            (*discord_id, rating)
        })
        .collect::<BTreeMap<_, _>>();

    set_match_fantasy_points(&fixture, &seeded);
    fixture.request_openskill_replay("late-latest-enrichment");
    let phase_two = fixture
        .repository
        .replay_openskill_atomic(Some(GUILD))
        .expect("replay weighted latest match");
    assert!(phase_two.errors.is_empty());
    assert_eq!(phase_two.matches_with_fantasy, 1);

    let mut moved = false;
    for discord_id in ids {
        let stored: (f64, f64, f64, f64, Option<f64>) = fixture
            .connection()
            .query_row(
                "SELECT p.os_mu,p.os_sigma,rh.os_mu_after,rh.os_sigma_after,
                        rh.fantasy_weight
                 FROM players p
                 JOIN rating_history rh
                   ON rh.discord_id=p.discord_id AND rh.guild_id=p.guild_id
                 WHERE p.discord_id=?1 AND p.guild_id=?2 AND rh.match_id=?3",
                params![discord_id, GUILD, seeded.match_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("load weighted live/history rating");
        approx(stored.0, stored.2);
        approx(stored.1, stored.3);
        assert!(stored.4.is_some());
        moved |= (stored.0 - phase_one_live[&discord_id].0).abs() > 1.0e-8;
    }
    assert!(
        moved,
        "fantasy weighting must alter at least one live rating"
    );
}

#[test]
fn test_enriching_old_match_replays_later_history() {
    let fixture = Fixture::new();
    let first = fixture.seed_match_at(12_700, NOW - 1, MatchSide::Radiant);
    let second = fixture.add_match_for_teams(&first, NOW, MatchSide::Radiant);
    fixture.request_openskill_replay("two-match-phase-one");
    let baseline = fixture
        .repository
        .replay_openskill_atomic(Some(GUILD))
        .expect("establish two equal-weight matches");
    assert!(baseline.errors.is_empty());
    let target = first.radiant_ids[0];
    let live_before: f64 = fixture
        .connection()
        .query_row(
            "SELECT os_mu FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![target, GUILD],
            |row| row.get(0),
        )
        .expect("load live rating before late enrichment");

    set_match_fantasy_points(&fixture, &first);
    fixture.request_openskill_replay("late-old-enrichment");
    let replayed = fixture
        .repository
        .replay_openskill_atomic(Some(GUILD))
        .expect("propagate old enrichment through later history");
    assert!(replayed.errors.is_empty());
    assert_eq!(replayed.matches_processed, 2);
    assert_eq!(replayed.matches_with_fantasy, 1);
    assert_eq!(replayed.matches_equal_weight, 1);

    let ids = first
        .radiant_ids
        .iter()
        .chain(&first.dire_ids)
        .copied()
        .collect::<Vec<_>>();
    for discord_id in ids {
        let first_after: (f64, f64, Option<f64>) = fixture
            .connection()
            .query_row(
                "SELECT os_mu_after,os_sigma_after,fantasy_weight
                 FROM rating_history
                 WHERE match_id=?1 AND guild_id=?2 AND discord_id=?3",
                params![first.match_id, GUILD, discord_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("load enriched old history");
        let second_history: (f64, f64, f64, f64) = fixture
            .connection()
            .query_row(
                "SELECT os_mu_before,os_sigma_before,os_mu_after,os_sigma_after
                 FROM rating_history
                 WHERE match_id=?1 AND guild_id=?2 AND discord_id=?3",
                params![second.match_id, GUILD, discord_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("load propagated later history");
        let live: (f64, f64) = fixture
            .connection()
            .query_row(
                "SELECT os_mu,os_sigma FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, GUILD],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("load propagated live rating");
        assert!(first_after.2.is_some());
        approx(first_after.0, second_history.0);
        approx(first_after.1, second_history.1);
        approx(live.0, second_history.2);
        approx(live.1, second_history.3);
    }
    let live_after: f64 = fixture
        .connection()
        .query_row(
            "SELECT os_mu FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![target, GUILD],
            |row| row.get(0),
        )
        .expect("load live rating after late enrichment");
    assert!((live_after - live_before).abs() > 1.0e-8);
}

#[test]
fn test_replay_persist_skips_unchanged_history_rows() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(11_700);
    let target = seeded.radiant_ids[0];
    let system = CamaOpenSkillSystem::default();
    let fingerprint = system.algorithm_fingerprint();
    let history = [ReplayHistoryRow {
        match_id: seeded.match_id,
        match_date: Value::Integer(NOW),
        discord_id: target,
        team_number: 1,
        won: true,
        before: OpenSkillRating::new(35.0, 8.0),
        after: OpenSkillRating::new(36.0, 8.0),
        fantasy_weight: None,
    }];
    let predictions = [ReplayPrediction {
        match_id: seeded.match_id,
        raw_probability: 0.5,
        calibrated_probability: 0.5,
    }];
    let current = BTreeMap::from([(target, OpenSkillRating::new(36.0, 8.0))]);

    let mut connection = fixture.connection();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("begin replay persistence proof");
    persist_openskill_replay(
        &transaction,
        GUILD,
        &history,
        &predictions,
        &current,
        &fingerprint,
    )
    .expect("persist first replay");
    transaction
        .execute_batch(
            "CREATE TEMP TABLE replay_write_audit(kind TEXT NOT NULL);
             CREATE TEMP TRIGGER audit_history_update
             AFTER UPDATE ON rating_history BEGIN
               INSERT INTO replay_write_audit(kind) VALUES('history-update');
             END;
             CREATE TEMP TRIGGER audit_history_insert
             AFTER INSERT ON rating_history BEGIN
               INSERT INTO replay_write_audit(kind) VALUES('history-insert');
             END;
             CREATE TEMP TRIGGER audit_prediction_update
             AFTER UPDATE ON match_predictions BEGIN
               INSERT INTO replay_write_audit(kind) VALUES('prediction-update');
             END;
             CREATE TEMP TRIGGER audit_prediction_insert
             AFTER INSERT ON match_predictions BEGIN
               INSERT INTO replay_write_audit(kind) VALUES('prediction-insert');
             END;",
        )
        .expect("install replay write audit");

    persist_openskill_replay(
        &transaction,
        GUILD,
        &history,
        &predictions,
        &current,
        &fingerprint,
    )
    .expect("persist identical replay");
    let writes: i64 = transaction
        .query_row("SELECT COUNT(*) FROM replay_write_audit", [], |row| {
            row.get(0)
        })
        .expect("count replay writes");
    assert_eq!(writes, 0);
    transaction.rollback().expect("roll back persistence proof");
}

#[test]
fn test_runtime_replay_refreshes_predictions_and_fingerprint() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(11_800);
    fixture
        .connection()
        .execute(
            "INSERT INTO match_predictions (
                 match_id,openskill_radiant_win_prob,
                 openskill_raw_radiant_win_prob,
                 openskill_algorithm_version,openskill_algorithm_fingerprint
             ) VALUES (?1,0.99,0.99,NULL,NULL)",
            [seeded.match_id],
        )
        .expect("seed stale OpenSkill prediction");
    fixture.request_openskill_replay("prediction-refresh");

    let summary = fixture
        .repository
        .replay_openskill_atomic(Some(GUILD))
        .expect("replay OpenSkill history");
    assert!(summary.errors.is_empty());
    let stored: (f64, f64, i64, String) = fixture
        .connection()
        .query_row(
            "SELECT openskill_radiant_win_prob,
                    openskill_raw_radiant_win_prob,
                    openskill_algorithm_version,
                    openskill_algorithm_fingerprint
               FROM match_predictions WHERE match_id=?1",
            [seeded.match_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("load refreshed prediction");
    assert!((stored.0 - 0.5).abs() < 1.0e-6);
    assert!((stored.1 - 0.5).abs() < 1.0e-6);
    assert_eq!(stored.2, OPENSKILL_REPLAY_ALGORITHM_VERSION);
    assert_eq!(
        stored.3,
        CamaOpenSkillSystem::default().algorithm_fingerprint()
    );
}

#[test]
fn test_admin_mu_and_sigma_events_survive_full_replay() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(11_900);
    let target = seeded.radiant_ids[0];
    let connection = fixture.connection();
    connection
        .execute(
            "UPDATE matches SET match_date=datetime(?1,'unixepoch') WHERE match_id=?2",
            params![NOW, seeded.match_id],
        )
        .expect("normalize match timestamp for ordered replay");
    connection
        .execute(
            "INSERT INTO openskill_rating_events (
                 guild_id,discord_id,event_type,value,event_at
             ) VALUES (?1,?2,'set_mu',60.0,datetime(?3,'unixepoch'))",
            params![GUILD, target, NOW + 1],
        )
        .expect("record administrator mu event");
    connection
        .execute(
            "INSERT INTO openskill_rating_events (
                 guild_id,discord_id,event_type,value,event_at
             ) VALUES (?1,?2,'add_sigma',2.5,datetime(?3,'unixepoch'))",
            params![GUILD, target, NOW + 2],
        )
        .expect("record administrator sigma event");
    drop(connection);
    fixture.request_openskill_replay("admin-events");

    let summary = fixture
        .repository
        .replay_openskill_atomic(Some(GUILD))
        .expect("replay administrator events");
    assert!(summary.errors.is_empty());
    let stored: (f64, f64) = fixture
        .connection()
        .query_row(
            "SELECT os_mu,os_sigma FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![target, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("load replayed administrator events");
    approx(stored.0, 60.0);
    approx(stored.1, CamaOpenSkillSystem::DEFAULT_SIGMA);
}

#[test]
fn test_recalibration_sigma_event_survives_full_replay() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(12_000);
    let target = seeded.radiant_ids[0];
    let connection = fixture.connection();
    connection
        .execute(
            "UPDATE matches SET match_date=datetime(?1,'unixepoch') WHERE match_id=?2",
            params![NOW, seeded.match_id],
        )
        .expect("normalize match timestamp for ordered replay");
    connection
        .execute(
            "INSERT INTO openskill_rating_events (
                 guild_id,discord_id,event_type,value,event_at
             ) VALUES (?1,?2,'set_sigma',7.5,datetime(?3,'unixepoch'))",
            params![GUILD, target, NOW + 1],
        )
        .expect("record recalibration sigma event");
    drop(connection);
    fixture.request_openskill_replay("recalibration-event");

    let summary = fixture
        .repository
        .replay_openskill_atomic(Some(GUILD))
        .expect("replay recalibration event");
    assert!(summary.errors.is_empty());
    let sigma: f64 = fixture
        .connection()
        .query_row(
            "SELECT os_sigma FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![target, GUILD],
            |row| row.get(0),
        )
        .expect("load replayed sigma");
    approx(sigma, 7.5);
}

#[test]
fn test_correction_finishes_after_partial_win_bonus_failure() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(14_000);
    let first = seeded.dire_ids[0];
    let second = seeded.dire_ids[1];
    fixture
        .connection()
        .execute_batch(&format!(
            "CREATE TRIGGER fail_second_bonus
             BEFORE UPDATE OF jopacoin_balance ON players
             WHEN NEW.discord_id={second}
              AND (SELECT source FROM economy_ledger_context WHERE id=1)='match_win_bonus'
             BEGIN SELECT RAISE(ABORT,'database is locked'); END;"
        ))
        .unwrap();
    assert!(
        fixture
            .repository
            .credit_resolved_win_bonus(seeded.match_id, Some(GUILD), first, WIN_REWARD)
            .unwrap()
    );
    fixture
        .repository
        .snapshot_win_bonus(seeded.match_id, Some(GUILD), first, WIN_REWARD)
        .unwrap();
    assert!(
        fixture
            .repository
            .credit_resolved_win_bonus(seeded.match_id, Some(GUILD), second, WIN_REWARD)
            .is_err()
    );
    assert_eq!(
        fixture
            .repository
            .current_winner(seeded.match_id, Some(GUILD))
            .unwrap(),
        MatchSide::Radiant
    );
    fixture
        .connection()
        .execute("DROP TRIGGER fail_second_bonus", [])
        .unwrap();
    for &discord_id in &seeded.dire_ids {
        if fixture
            .repository
            .credit_resolved_win_bonus(seeded.match_id, Some(GUILD), discord_id, WIN_REWARD)
            .unwrap()
        {
            fixture
                .repository
                .snapshot_win_bonus(seeded.match_id, Some(GUILD), discord_id, WIN_REWARD)
                .unwrap();
        }
    }
    let debits = seeded
        .radiant_ids
        .iter()
        .copied()
        .map(|discord_id| (discord_id, WIN_REWARD))
        .collect();
    fixture
        .repository
        .reverse_win_bonuses_atomic(seeded.match_id, Some(GUILD), &debits)
        .unwrap();
    fixture.claim(&seeded, MatchSide::Dire, "partial-bonus");
    fixture.apply_core(
        &seeded,
        MatchSide::Radiant,
        MatchSide::Dire,
        "partial-bonus",
        &flat_updates(&seeded, MatchSide::Dire),
    );
    for discord_id in seeded.radiant_ids {
        assert_eq!(fixture.balance(discord_id), 100);
    }
    for discord_id in seeded.dire_ids {
        assert_eq!(fixture.balance(discord_id), 115);
    }
}

#[test]
fn test_win_bonus_reversal_marks_rows_and_never_double_debits() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(15_000);
    let first = seeded
        .radiant_ids
        .iter()
        .copied()
        .map(|discord_id| (discord_id, WIN_REWARD))
        .collect::<BTreeMap<_, _>>();
    fixture
        .repository
        .reverse_win_bonuses_atomic(seeded.match_id, Some(GUILD), &first)
        .unwrap();
    let after_first = seeded
        .radiant_ids
        .iter()
        .map(|&discord_id| (discord_id, fixture.balance(discord_id)))
        .collect::<BTreeMap<_, _>>();
    let recomputed = seeded
        .radiant_ids
        .iter()
        .filter_map(|&discord_id| {
            fixture
                .win_bonus(seeded.match_id, discord_id)
                .filter(|amount| *amount > 0)
                .map(|amount| (discord_id, amount))
        })
        .collect::<BTreeMap<_, _>>();
    assert!(recomputed.is_empty());
    fixture
        .repository
        .reverse_win_bonuses_atomic(seeded.match_id, Some(GUILD), &recomputed)
        .unwrap();
    for &discord_id in &seeded.radiant_ids {
        assert_eq!(fixture.win_bonus(seeded.match_id, discord_id), Some(0));
        assert_eq!(fixture.balance(discord_id), after_first[&discord_id]);
    }
}

#[test]
fn test_correcting_older_match_preserves_current_glicko() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match_at(7_000, NOW - 100, MatchSide::Radiant);
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO matches(guild_id,winning_team,match_date,win_reward_jc,betting_mode)
             VALUES (?1,1,?2,?3,'pool')",
            params![GUILD, NOW, WIN_REWARD],
        )
        .unwrap();
    let later_match = connection.last_insert_rowid();
    drop(connection);
    for &discord_id in seeded.radiant_ids.iter().chain(&seeded.dire_ids) {
        fixture
            .connection()
            .execute(
                "INSERT INTO rating_history (
                     discord_id,rating,rating_before,rd_before,rd_after,
                     volatility_before,volatility_after,won,match_id,guild_id
                 ) VALUES (?1,1600.0,1520.0,80.0,78.0,0.06,0.06,1,?2,?3)",
                params![discord_id, later_match, GUILD],
            )
            .unwrap();
        fixture
            .connection()
            .execute(
                "UPDATE players SET glicko_rating=1600.0
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, GUILD],
            )
            .unwrap();
    }
    fixture.claim(&seeded, MatchSide::Dire, "older");
    fixture.apply_core(
        &seeded,
        MatchSide::Radiant,
        MatchSide::Dire,
        "older",
        &flat_updates(&seeded, MatchSide::Dire),
    );
    for &discord_id in seeded.radiant_ids.iter().chain(&seeded.dire_ids) {
        let live: f64 = fixture
            .connection()
            .query_row(
                "SELECT glicko_rating FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, GUILD],
                |row| row.get(0),
            )
            .unwrap();
        approx(live, 1600.0);
    }
    let corrected_dire_wins: i64 = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM rating_history
             WHERE match_id=?1 AND discord_id IN (?2,?3,?4,?5,?6) AND won=1",
            params![
                seeded.match_id,
                seeded.dire_ids[0],
                seeded.dire_ids[1],
                seeded.dire_ids[2],
                seeded.dire_ids[3],
                seeded.dire_ids[4]
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(corrected_dire_wins, 5);
}

#[test]
fn test_ledger_marks_the_bonus_as_paid_within_the_credit_transaction() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(8_100);
    let player = seeded.dire_ids[0];
    assert!(
        !fixture
            .repository
            .win_bonus_credited_ids(seeded.match_id, Some(GUILD))
            .unwrap()
            .contains(&player)
    );
    assert!(
        fixture
            .repository
            .credit_resolved_win_bonus(seeded.match_id, Some(GUILD), player, WIN_REWARD)
            .unwrap()
    );
    assert!(
        fixture
            .repository
            .win_bonus_credited_ids(seeded.match_id, Some(GUILD))
            .unwrap()
            .contains(&player)
    );
    assert!(
        !fixture
            .repository
            .win_bonus_credited_ids(seeded.match_id + 1, Some(GUILD))
            .unwrap()
            .contains(&player)
    );
}

#[test]
fn test_correction_retry_does_not_repay_a_credited_winner() {
    let fixture = Fixture::new();
    let seeded = fixture.seed_match(8_200);
    let victim = seeded.dire_ids[0];
    assert!(
        fixture
            .repository
            .credit_resolved_win_bonus(seeded.match_id, Some(GUILD), victim, WIN_REWARD)
            .unwrap()
    );
    let after_first = fixture.balance(victim);
    assert_eq!(fixture.win_bonus(seeded.match_id, victim), None);
    assert!(
        !fixture
            .repository
            .credit_resolved_win_bonus(seeded.match_id, Some(GUILD), victim, WIN_REWARD)
            .unwrap()
    );
    assert_eq!(fixture.balance(victim), after_first);
}

const FIXTURE_SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=OFF;
CREATE TABLE players (
    discord_id INTEGER NOT NULL,
    guild_id INTEGER NOT NULL DEFAULT 0,
    discord_username TEXT NOT NULL,
    initial_mmr INTEGER,
    jopacoin_balance INTEGER DEFAULT 0,
    lowest_balance_ever INTEGER,
    wins INTEGER DEFAULT 0,
    losses INTEGER DEFAULT 0,
    glicko_rating REAL,
    glicko_rd REAL,
    glicko_volatility REAL,
    os_mu REAL,
    os_sigma REAL,
    os_rating_version INTEGER,
    os_algorithm_fingerprint TEXT,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY(discord_id,guild_id)
);
CREATE TABLE matches (
    match_id INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id INTEGER NOT NULL DEFAULT 0,
    winning_team INTEGER,
    match_date INTEGER NOT NULL,
    team1_players TEXT,
    team2_players TEXT,
    win_reward_jc INTEGER,
    betting_mode TEXT,
    bonuses_paid INTEGER DEFAULT 0
);
CREATE TABLE match_participants (
    match_id INTEGER NOT NULL,
    discord_id INTEGER NOT NULL,
    team_number INTEGER,
    won INTEGER,
    side TEXT,
    guild_id INTEGER NOT NULL DEFAULT 0,
    fantasy_points REAL,
    bonus_jc INTEGER,
    win_bonus_jc INTEGER,
    PRIMARY KEY(match_id,discord_id)
);
CREATE TABLE rating_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    discord_id INTEGER NOT NULL,
    rating REAL,
    rating_before REAL,
    rd_before REAL,
    rd_after REAL,
    volatility_before REAL,
    volatility_after REAL,
    team_number INTEGER,
    won INTEGER,
    match_id INTEGER,
    timestamp INTEGER DEFAULT 0,
    os_mu_before REAL,
    os_mu_after REAL,
    os_sigma_before REAL,
    os_sigma_after REAL,
    fantasy_weight REAL,
    streak_length INTEGER,
    streak_multiplier REAL,
    streak_multiplier_per_game REAL,
    streak_threshold INTEGER,
    base_rating_delta_multiplier REAL,
    low_priority_gain_multiplier REAL,
    guild_id INTEGER NOT NULL DEFAULT 0,
    os_algorithm_version INTEGER,
    os_algorithm_fingerprint TEXT
);
CREATE TABLE player_pairings (
    guild_id INTEGER NOT NULL,
    player1_id INTEGER NOT NULL,
    player2_id INTEGER NOT NULL,
    games_together INTEGER NOT NULL DEFAULT 0,
    wins_together INTEGER NOT NULL DEFAULT 0,
    games_against INTEGER NOT NULL DEFAULT 0,
    player1_wins_against INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY(guild_id,player1_id,player2_id)
);
CREATE TABLE match_corrections (
    correction_id INTEGER PRIMARY KEY AUTOINCREMENT,
    match_id INTEGER NOT NULL,
    old_winning_team INTEGER NOT NULL,
    new_winning_team INTEGER NOT NULL,
    corrected_by INTEGER NOT NULL,
    corrected_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE match_correction_claims (
    match_id INTEGER PRIMARY KEY,
    guild_id INTEGER NOT NULL,
    old_winning_team INTEGER NOT NULL,
    new_winning_team INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK(state IN ('pending','core_applied')),
    owner_token TEXT,
    claimed_at INTEGER NOT NULL DEFAULT 0,
    correction_id INTEGER
);
CREATE TABLE bets (
    bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
    discord_id INTEGER NOT NULL,
    match_id INTEGER NOT NULL,
    team_bet_on TEXT NOT NULL,
    amount INTEGER NOT NULL,
    leverage INTEGER DEFAULT 1,
    payout INTEGER,
    bet_time INTEGER NOT NULL
);
CREATE TABLE bet_settlement_taxes (
    match_id INTEGER NOT NULL,
    guild_id INTEGER NOT NULL,
    discord_id INTEGER NOT NULL,
    vanity_tax INTEGER NOT NULL,
    PRIMARY KEY(match_id,guild_id,discord_id)
);
CREATE TABLE economy_ledger_context (
    id INTEGER PRIMARY KEY CHECK(id=1),
    source TEXT,
    actor_id INTEGER,
    related_type TEXT,
    related_id TEXT,
    reason TEXT,
    metadata TEXT
);
CREATE TABLE economy_ledger_entries (
    ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id INTEGER NOT NULL,
    account_type TEXT NOT NULL,
    account_id INTEGER,
    delta INTEGER NOT NULL,
    balance_before INTEGER NOT NULL,
    balance_after INTEGER NOT NULL,
    source TEXT NOT NULL,
    actor_id INTEGER,
    related_type TEXT,
    related_id TEXT,
    reason TEXT,
    metadata TEXT,
    created_at INTEGER NOT NULL DEFAULT 0
);
CREATE TRIGGER correction_balance_ledger
AFTER UPDATE OF jopacoin_balance ON players
WHEN COALESCE(OLD.jopacoin_balance,0) != COALESCE(NEW.jopacoin_balance,0)
BEGIN
    INSERT INTO economy_ledger_entries (
        guild_id,account_type,account_id,delta,balance_before,balance_after,
        source,actor_id,related_type,related_id,reason,metadata
    ) VALUES (
        NEW.guild_id,'player',NEW.discord_id,
        COALESCE(NEW.jopacoin_balance,0)-COALESCE(OLD.jopacoin_balance,0),
        COALESCE(OLD.jopacoin_balance,0),COALESCE(NEW.jopacoin_balance,0),
        COALESCE((SELECT source FROM economy_ledger_context WHERE id=1),'balance_update'),
        (SELECT actor_id FROM economy_ledger_context WHERE id=1),
        (SELECT related_type FROM economy_ledger_context WHERE id=1),
        (SELECT related_id FROM economy_ledger_context WHERE id=1),
        (SELECT reason FROM economy_ledger_context WHERE id=1),
        (SELECT metadata FROM economy_ledger_context WHERE id=1)
    );
END;
CREATE TABLE openskill_replay_jobs (
    guild_id INTEGER PRIMARY KEY,
    reason TEXT NOT NULL,
    requested_at TIMESTAMP NOT NULL,
    last_error TEXT
);
CREATE TABLE openskill_rating_revisions (
    guild_id INTEGER PRIMARY KEY,
    revision INTEGER NOT NULL,
    updated_at TIMESTAMP NOT NULL
);
CREATE TABLE openskill_rating_events (
    event_id INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id INTEGER NOT NULL DEFAULT 0,
    discord_id INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    value REAL NOT NULL,
    event_at TIMESTAMP NOT NULL
);
CREATE TABLE match_predictions (
    match_id INTEGER PRIMARY KEY,
    openskill_radiant_win_prob REAL,
    openskill_raw_radiant_win_prob REAL,
    openskill_algorithm_version INTEGER,
    openskill_algorithm_fingerprint TEXT
);
CREATE TABLE low_priority_state (
    discord_id INTEGER NOT NULL,
    guild_id INTEGER NOT NULL,
    active INTEGER NOT NULL,
    PRIMARY KEY(discord_id,guild_id)
);
"#;
