use std::collections::{BTreeMap, BTreeSet};

use cama_db::match_recording_repository::{BetDistributions, JcChange};

use super::*;

const GUILD: GuildId = 42;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RecordedCall {
    guild_id: GuildId,
    radiant_ids: Vec<DiscordId>,
    dire_ids: Vec<DiscordId>,
    excluded_ids: Vec<DiscordId>,
    winning_team: TeamSide,
}

#[derive(Clone, Debug, Default)]
struct MemoryRecorder {
    calls: Vec<RecordedCall>,
    fail_next: bool,
}

impl MatchRecorder for MemoryRecorder {
    fn record_match(
        &mut self,
        guild_id: GuildId,
        pending: &PendingMatch,
        winning_team: TeamSide,
    ) -> Result<MatchFinalization, String> {
        if self.fail_next {
            self.fail_next = false;
            return Err("injected backend failure".to_owned());
        }
        let winning_player_ids = if winning_team == TeamSide::Radiant {
            pending.radiant_team_ids.clone()
        } else {
            pending.dire_team_ids.clone()
        };
        let losing_player_ids = if winning_team == TeamSide::Radiant {
            pending.dire_team_ids.clone()
        } else {
            pending.radiant_team_ids.clone()
        };
        let excluded_player_ids = pending.all_excluded_ids();
        self.calls.push(RecordedCall {
            guild_id,
            radiant_ids: pending.radiant_team_ids.clone(),
            dire_ids: pending.dire_team_ids.clone(),
            excluded_ids: excluded_player_ids.clone(),
            winning_team,
        });
        let jc_changes = winning_player_ids
            .iter()
            .copied()
            .map(|discord_id| (discord_id, JcChange { payout: 10, bet: 0 }))
            .chain(
                losing_player_ids
                    .iter()
                    .copied()
                    .map(|discord_id| (discord_id, JcChange { payout: 5, bet: 0 })),
            )
            .collect::<BTreeMap<_, _>>();
        Ok(MatchFinalization {
            match_id: i64::try_from(self.calls.len()).expect("call count fits i64"),
            winning_team,
            winning_player_ids,
            losing_player_ids,
            excluded_player_ids,
            updated_count: pending.radiant_team_ids.len() + pending.dire_team_ids.len(),
            bet_distributions: BetDistributions::default(),
            jc_changes,
            loan_repayments: Vec::new(),
        })
    }
}

fn shuffled_service(player_ids: &[DiscordId]) -> MatchRecordingService<MemoryRecorder> {
    let mut service = MatchRecordingService::new(MemoryRecorder::default());
    service
        .shuffle_players(GUILD, player_ids, &[], 1_000)
        .expect("valid lobby");
    service
}

#[derive(Clone, Debug, Default)]
struct EasterEggSource {
    players: BTreeMap<DiscordId, MilestonePlayer>,
    personal_bests: BTreeMap<DiscordId, i64>,
    pairings: Vec<PairingSnapshot>,
    player_reads: usize,
    personal_best_batches: usize,
    pairing_reads: usize,
}

impl MatchEasterEggSource for EasterEggSource {
    type Error = String;

    fn players_by_ids(
        &mut self,
        discord_ids: &[DiscordId],
        _guild_id: GuildId,
    ) -> Result<Vec<MilestonePlayer>, Self::Error> {
        self.player_reads += 1;
        Ok(discord_ids
            .iter()
            .filter_map(|discord_id| self.players.get(discord_id).copied())
            .collect())
    }

    fn update_personal_bests_batch(
        &mut self,
        candidates: &[(DiscordId, i64)],
        _guild_id: GuildId,
    ) -> Result<BTreeMap<DiscordId, i64>, Self::Error> {
        self.personal_best_batches += 1;
        let mut improved = BTreeMap::new();
        for (discord_id, streak) in candidates {
            let previous = self.personal_bests.get(discord_id).copied().unwrap_or(0);
            if *streak > previous {
                improved.insert(*discord_id, previous);
                self.personal_bests.insert(*discord_id, *streak);
            }
        }
        Ok(improved)
    }

    fn pairings_for_players(
        &mut self,
        _discord_ids: &[DiscordId],
        _guild_id: GuildId,
    ) -> Result<Vec<PairingSnapshot>, Self::Error> {
        self.pairing_reads += 1;
        Ok(self.pairings.clone())
    }
}

#[test]
fn test_match_easter_egg_batches_personal_streak_records() {
    let player_ids = [99_101, 99_102];
    let mut source = EasterEggSource {
        players: player_ids
            .into_iter()
            .map(|discord_id| {
                (
                    discord_id,
                    MilestonePlayer {
                        discord_id,
                        wins: 4,
                        losses: 1,
                    },
                )
            })
            .collect(),
        personal_bests: [(player_ids[0], 4), (player_ids[1], 6)]
            .into_iter()
            .collect(),
        ..EasterEggSource::default()
    };
    let streaks = [(player_ids[0], (5, 1.0)), (player_ids[1], (6, 1.0))]
        .into_iter()
        .collect();
    let eggs = collect_match_easter_eggs(
        &mut source,
        &player_ids,
        &[],
        &player_ids,
        &player_ids.into_iter().collect(),
        &streaks,
        GUILD,
    )
    .expect("collect easter eggs");

    assert_eq!(
        eggs.win_streak_records,
        vec![WinStreakRecord {
            discord_id: player_ids[0],
            current_streak: 5,
            previous_best: 4,
        }]
    );
    assert_eq!((source.player_reads, source.personal_best_batches), (1, 1));
    assert_eq!(source.personal_bests[&player_ids[0]], 5);
    assert_eq!(source.personal_bests[&player_ids[1]], 6);
}

#[test]
fn test_match_easter_egg_batches_rivalries_with_correct_perspective() {
    let dire_id = 99_200;
    let radiant_ids = [99_301, 99_302];
    let mut source = EasterEggSource {
        pairings: radiant_ids
            .into_iter()
            .map(|radiant_id| PairingSnapshot {
                player1_id: dire_id,
                player2_id: radiant_id,
                player1_wins: 2,
                games_against: 10,
            })
            .collect(),
        ..EasterEggSource::default()
    };
    let expected_ids = [dire_id, radiant_ids[0], radiant_ids[1]]
        .into_iter()
        .collect();
    let eggs = collect_match_easter_eggs(
        &mut source,
        &radiant_ids,
        &[dire_id],
        &radiant_ids,
        &expected_ids,
        &BTreeMap::new(),
        GUILD,
    )
    .expect("collect rivalries");

    assert_eq!(
        eggs.rivalries_detected,
        vec![
            RivalryRecord {
                player1_id: radiant_ids[0],
                player2_id: dire_id,
                games_against: 10,
                winrate_vs: 80.0,
            },
            RivalryRecord {
                player1_id: radiant_ids[1],
                player2_id: dire_id,
                games_against: 10,
                winrate_vs: 80.0,
            },
        ]
    );
    assert_eq!(source.pairing_reads, 1);
}

#[test]
fn test_team_number_validation() {
    let players = (92_001..92_011).collect::<Vec<_>>();
    let mut service = shuffled_service(&players);
    assert_eq!(
        service.record_match("team1", GUILD),
        Err(MatchRecordingError::InvalidWinningTeam)
    );
    assert!(service.recorder().calls.is_empty());
    assert!(service.get_last_shuffle(GUILD).is_some());

    let result = service
        .record_match("radiant", GUILD)
        .expect("valid retry records");
    assert_eq!(result.winning_player_ids, players[..5]);
    assert!(service.get_last_shuffle(GUILD).is_none());
    assert_eq!(
        service.record_match("radiant", GUILD),
        Err(MatchRecordingError::NoRecentShuffle)
    );
}

#[test]
fn test_max_lobby_14_players_4_excluded() {
    let player_ids = [
        6_001, 6_002, 6_003, 6_004, -1, -2, -3, -4, -5, -6, -7, -8, -9, -10,
    ];
    let mut service = MatchRecordingService::new(MemoryRecorder::default());
    let result = service
        .shuffle_players(GUILD, &player_ids, &[], 1_000)
        .expect("shuffle maximum lobby");
    let pending = service.get_last_shuffle(GUILD).expect("stored pending");

    assert_eq!(result.excluded_ids.len(), 4);
    assert_eq!(pending.excluded_player_ids, result.excluded_ids);
    let included = pending.participants();
    let excluded = result.excluded_ids.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(included.len(), 10);
    assert!(included.is_disjoint(&excluded));
    assert_eq!(
        included.union(&excluded).copied().collect::<BTreeSet<_>>(),
        player_ids.into_iter().collect()
    );
}

#[test]
fn test_has_admin_submission_transitions() {
    let players = (5_001..5_011).collect::<Vec<_>>();
    let mut service = shuffled_service(&players);
    assert!(!service.has_admin_submission(GUILD));

    service
        .add_record_submission(GUILD, 5_001, "radiant", false)
        .expect("lobby vote");
    assert!(!service.has_admin_submission(GUILD));
    assert!(!service.can_record_match(GUILD));

    service
        .add_record_submission(GUILD, 9_999, "radiant", true)
        .expect("admin vote");
    assert!(service.has_admin_submission(GUILD));
    assert!(service.can_record_match(GUILD));
    assert_eq!(service.get_non_admin_submission_count(GUILD), 1);
}

#[test]
fn test_admin_override_allows_immediate_recording() {
    let players = (5_101..5_111).collect::<Vec<_>>();
    let mut service = shuffled_service(&players);
    service
        .add_record_submission(GUILD, players[0], "radiant", false)
        .expect("first vote");
    assert!(!service.can_record_match(GUILD));
    let submission = service
        .add_record_submission(GUILD, 9_999, "radiant", true)
        .expect("admin override");
    assert!(submission.is_ready);
    assert_eq!(submission.non_admin_count, 1);

    let result = service
        .record_match("radiant", GUILD)
        .expect("record match");
    assert_eq!(result.winning_team, TeamSide::Radiant);
    assert_eq!(result.updated_count, 10);
    assert!(service.get_last_shuffle(GUILD).is_none());
    assert!(!service.can_record_match(GUILD));
    assert!(!service.has_admin_submission(GUILD));
}

#[test]
fn test_get_vote_counts_tracks_votes() {
    let players = (6_001..6_011).collect::<Vec<_>>();
    let mut service = shuffled_service(&players);
    assert_eq!(service.get_vote_counts(GUILD), VoteCounts::default());
    service
        .add_record_submission(GUILD, 9_999, "radiant", true)
        .expect("admin vote");
    assert_eq!(service.get_vote_counts(GUILD), VoteCounts::default());
    service
        .add_record_submission(GUILD, 6_001, "radiant", false)
        .expect("radiant vote");
    let submission = service
        .add_record_submission(GUILD, 6_002, "dire", false)
        .expect("dire vote");
    assert_eq!(
        submission.vote_counts,
        VoteCounts {
            radiant: 1,
            dire: 1
        }
    );
    service
        .add_record_submission(GUILD, 6_003, "radiant", false)
        .expect("second radiant vote");
    assert_eq!(
        service.get_vote_counts(GUILD),
        VoteCounts {
            radiant: 2,
            dire: 1
        }
    );
}

#[test]
fn test_first_to_3_radiant_wins() {
    let players = (6_101..6_111).collect::<Vec<_>>();
    let mut service = shuffled_service(&players);
    for (user_id, side) in [
        (6_101, "radiant"),
        (6_102, "dire"),
        (6_103, "radiant"),
        (6_104, "dire"),
    ] {
        service
            .add_record_submission(GUILD, user_id, side, false)
            .expect("vote");
    }
    assert!(!service.can_record_match(GUILD));
    assert_eq!(service.get_pending_record_result(GUILD), None);
    assert_eq!(service.get_non_admin_submission_count(GUILD), 4);
    let submission = service
        .add_record_submission(GUILD, 6_105, "radiant", false)
        .expect("deciding vote");
    assert!(submission.is_ready);
    assert_eq!(submission.result, Some(TeamSide::Radiant));
    assert_eq!(
        service.get_pending_record_result(GUILD),
        Some(TeamSide::Radiant)
    );
    assert_eq!(service.get_non_admin_submission_count(GUILD), 5);
}

#[test]
fn test_first_to_3_dire_wins() {
    let players = (6_201..6_211).collect::<Vec<_>>();
    let mut service = shuffled_service(&players);
    for (user_id, side) in [(6_201, "radiant"), (6_202, "dire"), (6_203, "dire")] {
        service
            .add_record_submission(GUILD, user_id, side, false)
            .expect("vote");
    }
    assert!(!service.can_record_match(GUILD));
    let submission = service
        .add_record_submission(GUILD, 6_204, "dire", false)
        .expect("deciding vote");
    assert!(submission.is_ready);
    assert_eq!(submission.result, Some(TeamSide::Dire));
    assert_eq!(
        service.get_pending_record_result(GUILD),
        Some(TeamSide::Dire)
    );
}

#[test]
fn test_user_cannot_change_vote() {
    let players = (6_301..6_311).collect::<Vec<_>>();
    let mut service = shuffled_service(&players);
    service
        .add_record_submission(GUILD, 6_301, "radiant", false)
        .expect("first vote");
    assert_eq!(
        service.add_record_submission(GUILD, 6_301, "dire", false),
        Err(MatchRecordingError::AlreadySubmitted)
    );
    service
        .add_record_submission(GUILD, 6_301, "radiant", false)
        .expect("same vote is idempotent");
    assert_eq!(
        service.get_vote_counts(GUILD),
        VoteCounts {
            radiant: 1,
            dire: 0
        }
    );
}

#[test]
fn test_first_to_3_records_correct_winner() {
    let players = (6_401..6_411).collect::<Vec<_>>();
    let mut service = shuffled_service(&players);
    for (user_id, side) in [
        (6_401, "dire"),
        (6_402, "radiant"),
        (6_403, "dire"),
        (6_404, "radiant"),
        (6_405, "radiant"),
    ] {
        service
            .add_record_submission(GUILD, user_id, side, false)
            .expect("vote");
    }
    assert_eq!(
        service.get_pending_record_result(GUILD),
        Some(TeamSide::Radiant)
    );
    let result = service
        .record_match("radiant", GUILD)
        .expect("record voted result");
    assert_eq!(result.winning_team, TeamSide::Radiant);
    assert_eq!(service.recorder().calls[0].winning_team, TeamSide::Radiant);
}

#[test]
fn test_non_admin_abort_requires_four_lobby_votes() {
    let players = (7_001..7_011).collect::<Vec<_>>();
    let mut service = shuffled_service(&players);
    assert!(!service.can_abort_match(GUILD));
    assert_eq!(
        service.add_abort_submission(GUILD, 9_999, false),
        Err(MatchRecordingError::AbortVoterNotEligible)
    );
    assert_eq!(service.get_abort_submission_count(GUILD), 0);
    for user_id in &players[..3] {
        service
            .add_abort_submission(GUILD, *user_id, false)
            .expect("abort vote");
    }
    assert!(!service.can_abort_match(GUILD));
    let submission = service
        .add_abort_submission(GUILD, players[3], false)
        .expect("fourth abort vote");
    assert!(submission.is_ready);
    assert!(service.can_abort_match(GUILD));
    service.clear_last_shuffle(GUILD);
    assert!(!service.can_abort_match(GUILD));
}

#[test]
fn test_admin_abort_overrides_minimum() {
    let players = (7_101..7_111).collect::<Vec<_>>();
    let mut service = shuffled_service(&players);
    let submission = service
        .add_abort_submission(GUILD, 9_999, true)
        .expect("admin abort");
    assert!(submission.is_ready);
    assert!(service.can_abort_match(GUILD));
    assert_eq!(
        submission.non_admin_count,
        service.get_abort_submission_count(GUILD)
    );
}

#[test]
fn test_excluded_players_validation() {
    let players = (97_001..97_012).collect::<Vec<_>>();
    let mut service = shuffled_service(&players);
    let pending = service
        .get_last_shuffle_mut(GUILD)
        .expect("stored pending match");
    pending.excluded_player_ids = vec![pending.radiant_team_ids[0], players[10]];
    assert_eq!(
        service.record_match("radiant", GUILD),
        Err(MatchRecordingError::ExcludedPlayersInTeams)
    );
    assert!(service.recorder().calls.is_empty());
    assert!(service.get_last_shuffle(GUILD).is_some());

    service.recorder_mut().fail_next = true;
    service
        .get_last_shuffle_mut(GUILD)
        .expect("pending retained")
        .excluded_player_ids = vec![players[10]];
    assert!(matches!(
        service.record_match("radiant", GUILD),
        Err(MatchRecordingError::Backend(_))
    ));
    assert!(service.get_last_shuffle(GUILD).is_some());
}
