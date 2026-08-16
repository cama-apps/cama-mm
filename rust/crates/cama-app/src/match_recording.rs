//! Match-recording orchestration independent from Discord.
//!
//! The module owns pending-lobby validation, first-to-three result voting,
//! four-vote abort policy, admin overrides, and batched match easter-egg
//! collection. Durable finalization is a port; the SQLite implementation
//! delegates to `cama-db`'s existing-schema repository.

use std::collections::{BTreeMap, BTreeSet};

use cama_db::match_recording_repository::{
    MatchFinalization, MatchRecordRequest, MatchRecordingRepository, TeamSide,
};
use thiserror::Error;

pub type DiscordId = i64;
pub type GuildId = i64;

pub const RESULT_VOTE_THRESHOLD: usize = 3;
pub const ABORT_VOTE_THRESHOLD: usize = 4;
pub const MATCH_PLAYER_COUNT: usize = 10;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RecordVote {
    user_id: DiscordId,
    result: TeamSide,
    is_admin: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AbortVote {
    user_id: DiscordId,
    is_admin: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingMatch {
    pub radiant_team_ids: Vec<DiscordId>,
    pub dire_team_ids: Vec<DiscordId>,
    pub excluded_player_ids: Vec<DiscordId>,
    pub conditional_excluded_ids: Vec<DiscordId>,
    pub pending_bet_since: i64,
    record_votes: Vec<RecordVote>,
    abort_votes: Vec<AbortVote>,
    pending_record_result: Option<TeamSide>,
    abort_ready: bool,
}

impl PendingMatch {
    #[must_use]
    pub fn new(
        radiant_team_ids: Vec<DiscordId>,
        dire_team_ids: Vec<DiscordId>,
        excluded_player_ids: Vec<DiscordId>,
        conditional_excluded_ids: Vec<DiscordId>,
        pending_bet_since: i64,
    ) -> Self {
        Self {
            radiant_team_ids,
            dire_team_ids,
            excluded_player_ids,
            conditional_excluded_ids,
            pending_bet_since,
            record_votes: Vec::new(),
            abort_votes: Vec::new(),
            pending_record_result: None,
            abort_ready: false,
        }
    }

    fn participants(&self) -> BTreeSet<DiscordId> {
        self.radiant_team_ids
            .iter()
            .chain(&self.dire_team_ids)
            .copied()
            .collect()
    }

    fn lobby_members(&self) -> BTreeSet<DiscordId> {
        self.radiant_team_ids
            .iter()
            .chain(&self.dire_team_ids)
            .chain(&self.excluded_player_ids)
            .chain(&self.conditional_excluded_ids)
            .copied()
            .collect()
    }

    fn all_excluded_ids(&self) -> Vec<DiscordId> {
        let mut seen = BTreeSet::new();
        self.excluded_player_ids
            .iter()
            .chain(&self.conditional_excluded_ids)
            .copied()
            .filter(|discord_id| seen.insert(*discord_id))
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShuffleResult {
    pub radiant_ids: Vec<DiscordId>,
    pub dire_ids: Vec<DiscordId>,
    pub excluded_ids: Vec<DiscordId>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VoteCounts {
    pub radiant: usize,
    pub dire: usize,
}

impl VoteCounts {
    fn add(&mut self, side: TeamSide) {
        match side {
            TeamSide::Radiant => self.radiant += 1,
            TeamSide::Dire => self.dire += 1,
        }
    }

    fn count(self, side: TeamSide) -> usize {
        match side {
            TeamSide::Radiant => self.radiant,
            TeamSide::Dire => self.dire,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubmissionSummary {
    pub is_ready: bool,
    pub non_admin_count: usize,
    pub result: Option<TeamSide>,
    pub vote_counts: VoteCounts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbortSubmissionSummary {
    pub is_ready: bool,
    pub non_admin_count: usize,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum MatchRecordingError {
    #[error("No recent shuffle found")]
    NoRecentShuffle,
    #[error("winning_team must be 'radiant' or 'dire'")]
    InvalidWinningTeam,
    #[error("at least ten distinct players are required")]
    InsufficientPlayers,
    #[error("duplicate player in lobby")]
    DuplicatePlayer,
    #[error("Excluded players detected in match teams")]
    ExcludedPlayersInTeams,
    #[error("result voter must be on one of the match teams")]
    ResultVoterNotEligible,
    #[error("abort voter must be in the shuffled lobby")]
    AbortVoterNotEligible,
    #[error("user already submitted a different result")]
    AlreadySubmitted,
    #[error("match finalization failed: {0}")]
    Backend(String),
}

/// Durable boundary used after all pending-state validation succeeds.
pub trait MatchRecorder {
    fn record_match(
        &mut self,
        guild_id: GuildId,
        pending: &PendingMatch,
        winning_team: TeamSide,
    ) -> Result<MatchFinalization, String>;
}

impl MatchRecorder for MatchRecordingRepository {
    fn record_match(
        &mut self,
        guild_id: GuildId,
        pending: &PendingMatch,
        winning_team: TeamSide,
    ) -> Result<MatchFinalization, String> {
        let all_excluded_ids = pending.all_excluded_ids();
        let mut request = MatchRecordRequest::standard(
            Some(guild_id),
            &pending.radiant_team_ids,
            &pending.dire_team_ids,
            winning_team,
        );
        request.rewarded_excluded_ids = &pending.excluded_player_ids;
        request.all_excluded_ids = &all_excluded_ids;
        request.pending_bet_since = pending.pending_bet_since;
        self.record_match_atomic(request)
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct MatchRecordingService<R> {
    recorder: R,
    pending: BTreeMap<GuildId, PendingMatch>,
}

impl<R> MatchRecordingService<R> {
    #[must_use]
    pub fn new(recorder: R) -> Self {
        Self {
            recorder,
            pending: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn recorder(&self) -> &R {
        &self.recorder
    }

    #[must_use]
    pub const fn recorder_mut(&mut self) -> &mut R {
        &mut self.recorder
    }

    pub fn set_pending_match(&mut self, guild_id: GuildId, pending: PendingMatch) {
        self.pending.insert(guild_id, pending);
    }

    #[must_use]
    pub fn get_last_shuffle(&self, guild_id: GuildId) -> Option<&PendingMatch> {
        self.pending.get(&guild_id)
    }

    pub fn get_last_shuffle_mut(&mut self, guild_id: GuildId) -> Option<&mut PendingMatch> {
        self.pending.get_mut(&guild_id)
    }

    pub fn clear_last_shuffle(&mut self, guild_id: GuildId) -> Option<PendingMatch> {
        self.pending.remove(&guild_id)
    }

    pub fn shuffle_players(
        &mut self,
        guild_id: GuildId,
        player_ids: &[DiscordId],
        conditional_excluded_ids: &[DiscordId],
        pending_bet_since: i64,
    ) -> Result<ShuffleResult, MatchRecordingError> {
        let distinct = player_ids.iter().copied().collect::<BTreeSet<_>>();
        if distinct.len() != player_ids.len() {
            return Err(MatchRecordingError::DuplicatePlayer);
        }
        if player_ids.len() < MATCH_PLAYER_COUNT {
            return Err(MatchRecordingError::InsufficientPlayers);
        }
        let radiant_ids = player_ids[..MATCH_PLAYER_COUNT / 2].to_vec();
        let dire_ids = player_ids[MATCH_PLAYER_COUNT / 2..MATCH_PLAYER_COUNT].to_vec();
        let excluded_ids = player_ids[MATCH_PLAYER_COUNT..].to_vec();
        self.pending.insert(
            guild_id,
            PendingMatch::new(
                radiant_ids.clone(),
                dire_ids.clone(),
                excluded_ids.clone(),
                conditional_excluded_ids.to_vec(),
                pending_bet_since,
            ),
        );
        Ok(ShuffleResult {
            radiant_ids,
            dire_ids,
            excluded_ids,
        })
    }

    #[must_use]
    pub fn has_admin_submission(&self, guild_id: GuildId) -> bool {
        self.pending
            .get(&guild_id)
            .is_some_and(|pending| pending.record_votes.iter().any(|vote| vote.is_admin))
    }

    #[must_use]
    pub fn get_vote_counts(&self, guild_id: GuildId) -> VoteCounts {
        let mut counts = VoteCounts::default();
        if let Some(pending) = self.pending.get(&guild_id) {
            for vote in pending.record_votes.iter().filter(|vote| !vote.is_admin) {
                counts.add(vote.result);
            }
        }
        counts
    }

    #[must_use]
    pub fn get_non_admin_submission_count(&self, guild_id: GuildId) -> usize {
        self.pending.get(&guild_id).map_or(0, |pending| {
            pending
                .record_votes
                .iter()
                .filter(|vote| !vote.is_admin)
                .count()
        })
    }

    #[must_use]
    pub fn get_pending_record_result(&self, guild_id: GuildId) -> Option<TeamSide> {
        self.pending
            .get(&guild_id)
            .and_then(|pending| pending.pending_record_result)
    }

    #[must_use]
    pub fn can_record_match(&self, guild_id: GuildId) -> bool {
        self.get_pending_record_result(guild_id).is_some()
    }

    pub fn add_record_submission(
        &mut self,
        guild_id: GuildId,
        user_id: DiscordId,
        result: &str,
        is_admin: bool,
    ) -> Result<SubmissionSummary, MatchRecordingError> {
        let result = TeamSide::parse(result).ok_or(MatchRecordingError::InvalidWinningTeam)?;
        let pending = self
            .pending
            .get_mut(&guild_id)
            .ok_or(MatchRecordingError::NoRecentShuffle)?;
        if !is_admin && !pending.participants().contains(&user_id) {
            return Err(MatchRecordingError::ResultVoterNotEligible);
        }
        if let Some(existing) = pending
            .record_votes
            .iter()
            .find(|submission| submission.user_id == user_id)
        {
            if existing.result != result || existing.is_admin != is_admin {
                return Err(MatchRecordingError::AlreadySubmitted);
            }
            return Ok(summarize_record_submissions(pending));
        }
        pending.record_votes.push(RecordVote {
            user_id,
            result,
            is_admin,
        });
        let reaches_threshold = !is_admin
            && pending.pending_record_result.is_none()
            && vote_counts(pending).count(result) >= RESULT_VOTE_THRESHOLD;
        if is_admin || reaches_threshold {
            pending.pending_record_result = Some(result);
        }
        Ok(summarize_record_submissions(pending))
    }

    #[must_use]
    pub fn get_abort_submission_count(&self, guild_id: GuildId) -> usize {
        self.pending.get(&guild_id).map_or(0, |pending| {
            pending
                .abort_votes
                .iter()
                .filter(|vote| !vote.is_admin)
                .count()
        })
    }

    #[must_use]
    pub fn can_abort_match(&self, guild_id: GuildId) -> bool {
        self.pending
            .get(&guild_id)
            .is_some_and(|pending| pending.abort_ready)
    }

    pub fn add_abort_submission(
        &mut self,
        guild_id: GuildId,
        user_id: DiscordId,
        is_admin: bool,
    ) -> Result<AbortSubmissionSummary, MatchRecordingError> {
        let pending = self
            .pending
            .get_mut(&guild_id)
            .ok_or(MatchRecordingError::NoRecentShuffle)?;
        if !is_admin && !pending.lobby_members().contains(&user_id) {
            return Err(MatchRecordingError::AbortVoterNotEligible);
        }
        if !pending
            .abort_votes
            .iter()
            .any(|submission| submission.user_id == user_id)
        {
            pending.abort_votes.push(AbortVote { user_id, is_admin });
        }
        let non_admin_count = pending
            .abort_votes
            .iter()
            .filter(|submission| !submission.is_admin)
            .count();
        pending.abort_ready = pending.abort_votes.iter().any(|vote| vote.is_admin)
            || non_admin_count >= ABORT_VOTE_THRESHOLD;
        Ok(AbortSubmissionSummary {
            is_ready: pending.abort_ready,
            non_admin_count,
        })
    }
}

impl<R: MatchRecorder> MatchRecordingService<R> {
    pub fn record_match(
        &mut self,
        winning_team: &str,
        guild_id: GuildId,
    ) -> Result<MatchFinalization, MatchRecordingError> {
        let winning_team =
            TeamSide::parse(winning_team).ok_or(MatchRecordingError::InvalidWinningTeam)?;
        let pending = self
            .pending
            .get(&guild_id)
            .cloned()
            .ok_or(MatchRecordingError::NoRecentShuffle)?;
        if !pending
            .participants()
            .is_disjoint(&pending.all_excluded_ids().into_iter().collect())
        {
            return Err(MatchRecordingError::ExcludedPlayersInTeams);
        }
        let result = self
            .recorder
            .record_match(guild_id, &pending, winning_team)
            .map_err(MatchRecordingError::Backend)?;
        self.pending.remove(&guild_id);
        Ok(result)
    }
}

fn vote_counts(pending: &PendingMatch) -> VoteCounts {
    let mut counts = VoteCounts::default();
    for vote in pending.record_votes.iter().filter(|vote| !vote.is_admin) {
        counts.add(vote.result);
    }
    counts
}

fn summarize_record_submissions(pending: &PendingMatch) -> SubmissionSummary {
    let vote_counts = vote_counts(pending);
    SubmissionSummary {
        is_ready: pending.pending_record_result.is_some(),
        non_admin_count: pending
            .record_votes
            .iter()
            .filter(|vote| !vote.is_admin)
            .count(),
        result: pending.pending_record_result,
        vote_counts,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MilestonePlayer {
    pub discord_id: DiscordId,
    pub wins: i64,
    pub losses: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairingSnapshot {
    pub player1_id: DiscordId,
    pub player2_id: DiscordId,
    pub player1_wins: i64,
    pub games_against: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WinStreakRecord {
    pub discord_id: DiscordId,
    pub current_streak: i64,
    pub previous_best: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RivalryRecord {
    pub player1_id: DiscordId,
    pub player2_id: DiscordId,
    pub games_against: i64,
    pub winrate_vs: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GamesMilestone {
    pub discord_id: DiscordId,
    pub games_played: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MatchEasterEggs {
    pub win_streak_records: Vec<WinStreakRecord>,
    pub rivalries_detected: Vec<RivalryRecord>,
    pub games_milestones: Vec<GamesMilestone>,
}

pub trait MatchEasterEggSource {
    type Error: std::fmt::Display;

    fn players_by_ids(
        &mut self,
        discord_ids: &[DiscordId],
        guild_id: GuildId,
    ) -> Result<Vec<MilestonePlayer>, Self::Error>;

    /// Atomically update every candidate and return the previous best only
    /// for candidates whose new streak is strictly greater.
    fn update_personal_bests_batch(
        &mut self,
        candidates: &[(DiscordId, i64)],
        guild_id: GuildId,
    ) -> Result<BTreeMap<DiscordId, i64>, Self::Error>;

    fn pairings_for_players(
        &mut self,
        discord_ids: &[DiscordId],
        guild_id: GuildId,
    ) -> Result<Vec<PairingSnapshot>, Self::Error>;
}

#[derive(Debug, Error, Eq, PartialEq)]
#[error("match easter-egg collection failed: {0}")]
pub struct MatchEasterEggError(pub String);

pub fn collect_match_easter_eggs<S: MatchEasterEggSource>(
    source: &mut S,
    radiant_team_ids: &[DiscordId],
    dire_team_ids: &[DiscordId],
    winning_ids: &[DiscordId],
    expected_ids: &BTreeSet<DiscordId>,
    streak_data: &BTreeMap<DiscordId, (i64, f64)>,
    guild_id: GuildId,
) -> Result<MatchEasterEggs, MatchEasterEggError> {
    let expected = expected_ids.iter().copied().collect::<Vec<_>>();
    let players = source
        .players_by_ids(&expected, guild_id)
        .map_err(|error| MatchEasterEggError(error.to_string()))?;
    let games_milestones = players
        .into_iter()
        .filter_map(|player| {
            let games_played = player.wins + player.losses;
            (games_played > 0 && games_played % 100 == 0).then_some(GamesMilestone {
                discord_id: player.discord_id,
                games_played,
            })
        })
        .collect();

    let streak_candidates = winning_ids
        .iter()
        .filter_map(|discord_id| {
            streak_data
                .get(discord_id)
                .map(|(streak, _)| (*discord_id, *streak))
        })
        .filter(|(_, streak)| *streak >= 5)
        .collect::<Vec<_>>();
    let previous_bests = source
        .update_personal_bests_batch(&streak_candidates, guild_id)
        .map_err(|error| MatchEasterEggError(error.to_string()))?;
    let win_streak_records = streak_candidates
        .into_iter()
        .filter_map(|(discord_id, current_streak)| {
            previous_bests
                .get(&discord_id)
                .copied()
                .map(|previous_best| WinStreakRecord {
                    discord_id,
                    current_streak,
                    previous_best,
                })
        })
        .collect();

    let pairings = if radiant_team_ids.is_empty() || dire_team_ids.is_empty() {
        Vec::new()
    } else {
        source
            .pairings_for_players(&expected, guild_id)
            .map_err(|error| MatchEasterEggError(error.to_string()))?
    };
    let lookup = pairings
        .into_iter()
        .map(|pairing| {
            (
                ordered_pair(pairing.player1_id, pairing.player2_id),
                pairing,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut rivalries_detected = Vec::new();
    for radiant_id in radiant_team_ids {
        for dire_id in dire_team_ids {
            let Some(pairing) = lookup.get(&ordered_pair(*radiant_id, *dire_id)) else {
                continue;
            };
            if pairing.games_against < 10 {
                continue;
            }
            let radiant_wins = if *radiant_id == pairing.player1_id {
                pairing.player1_wins
            } else {
                pairing.games_against - pairing.player1_wins
            };
            let winrate = 100.0 * radiant_wins as f64 / pairing.games_against as f64;
            if winrate >= 70.0 || winrate <= 30.0 {
                rivalries_detected.push(RivalryRecord {
                    player1_id: *radiant_id,
                    player2_id: *dire_id,
                    games_against: pairing.games_against,
                    winrate_vs: winrate,
                });
            }
        }
    }

    Ok(MatchEasterEggs {
        win_streak_records,
        rivalries_detected,
        games_milestones,
    })
}

fn ordered_pair(first: DiscordId, second: DiscordId) -> (DiscordId, DiscordId) {
    if first < second {
        (first, second)
    } else {
        (second, first)
    }
}

#[cfg(test)]
#[path = "match_recording/tests.rs"]
mod tests;
