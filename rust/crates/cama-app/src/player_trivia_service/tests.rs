use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
enum SessionCall {
    Start {
        discord_id: DiscordId,
        guild_id: GuildId,
        questions: Vec<PlayerTriviaQuestionRecord>,
        now: i64,
        cooldown_seconds: i64,
        bypass: bool,
    },
    Answer {
        session_id: i64,
        question_number: i64,
        selected_index: usize,
        reward: i64,
        answered_at: i64,
    },
    Finish {
        session_id: i64,
        status: String,
        completed_at: i64,
    },
    Cancel {
        session_id: i64,
    },
}

#[derive(Default)]
struct FakeRepository {
    snapshots: Mutex<BTreeMap<GuildId, PlayerTriviaSnapshot>>,
    queued_loads: Mutex<VecDeque<Result<PlayerTriviaSnapshot, String>>>,
    recent: Mutex<Vec<String>>,
    load_calls: Mutex<Vec<GuildId>>,
    recent_calls: Mutex<Vec<(DiscordId, GuildId, i64)>>,
    session_calls: Mutex<Vec<SessionCall>>,
}

impl FakeRepository {
    fn with_snapshot(guild_id: GuildId, snapshot: PlayerTriviaSnapshot) -> Arc<Self> {
        Arc::new(Self {
            snapshots: Mutex::new(BTreeMap::from([(guild_id, snapshot)])),
            ..Self::default()
        })
    }

    fn set_snapshot(&self, guild_id: GuildId, snapshot: PlayerTriviaSnapshot) {
        self.snapshots.lock().unwrap().insert(guild_id, snapshot);
    }

    fn set_recent(&self, keys: impl IntoIterator<Item = impl Into<String>>) {
        *self.recent.lock().unwrap() = keys.into_iter().map(Into::into).collect();
    }

    fn queue_load(&self, result: Result<PlayerTriviaSnapshot, String>) {
        self.queued_loads.lock().unwrap().push_back(result);
    }

    fn load_calls(&self) -> Vec<GuildId> {
        self.load_calls.lock().unwrap().clone()
    }

    fn recent_calls(&self) -> Vec<(DiscordId, GuildId, i64)> {
        self.recent_calls.lock().unwrap().clone()
    }

    fn session_calls(&self) -> Vec<SessionCall> {
        self.session_calls.lock().unwrap().clone()
    }
}

impl PlayerTriviaSnapshotRepository for FakeRepository {
    fn load_snapshot(&self, guild_id: GuildId) -> Result<PlayerTriviaSnapshot, String> {
        self.load_calls.lock().unwrap().push(guild_id);
        if let Some(result) = self.queued_loads.lock().unwrap().pop_front() {
            return result;
        }
        self.snapshots
            .lock()
            .unwrap()
            .get(&guild_id)
            .cloned()
            .ok_or_else(|| format!("missing guild {guild_id}"))
    }

    fn recent_question_keys(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
        since: i64,
    ) -> Result<Vec<String>, String> {
        self.recent_calls
            .lock()
            .unwrap()
            .push((discord_id, guild_id, since));
        Ok(self.recent.lock().unwrap().clone())
    }
}

impl PlayerTriviaSessionRepository for FakeRepository {
    type StartResult = i64;
    type AnswerResult = bool;
    type FinishResult = ();
    type CancelResult = bool;

    fn try_start_session(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
        questions: &[PlayerTriviaQuestionRecord],
        now: i64,
        cooldown_seconds: i64,
        bypass: bool,
    ) -> Result<Self::StartResult, String> {
        self.session_calls.lock().unwrap().push(SessionCall::Start {
            discord_id,
            guild_id,
            questions: questions.to_vec(),
            now,
            cooldown_seconds,
            bypass,
        });
        Ok(4)
    }

    fn settle_answer(
        &self,
        session_id: i64,
        question_number: i64,
        selected_index: usize,
        reward: i64,
        answered_at: i64,
    ) -> Result<Self::AnswerResult, String> {
        self.session_calls
            .lock()
            .unwrap()
            .push(SessionCall::Answer {
                session_id,
                question_number,
                selected_index,
                reward,
                answered_at,
            });
        Ok(true)
    }

    fn finish_session(
        &self,
        session_id: i64,
        status: &str,
        completed_at: i64,
    ) -> Result<Self::FinishResult, String> {
        self.session_calls
            .lock()
            .unwrap()
            .push(SessionCall::Finish {
                session_id,
                status: status.to_owned(),
                completed_at,
            });
        Ok(())
    }

    fn cancel_session_if_unanswered(&self, session_id: i64) -> Result<Self::CancelResult, String> {
        self.session_calls
            .lock()
            .unwrap()
            .push(SessionCall::Cancel { session_id });
        Ok(true)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct StableRandom;

impl PlayerTriviaRandom for StableRandom {
    fn next_index(&mut self, _upper: usize) -> usize {
        0
    }

    fn shuffle_enabled(&self) -> bool {
        false
    }
}

#[derive(Debug)]
struct ManualClock {
    now: Mutex<f64>,
}

impl ManualClock {
    fn new(now: f64) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    fn advance(&self, seconds: f64) {
        *self.now.lock().unwrap() += seconds;
    }
}

impl MonotonicClock for ManualClock {
    fn now(&self) -> f64 {
        *self.now.lock().unwrap()
    }
}

#[derive(Debug, Default)]
struct CountingClock {
    calls: Mutex<usize>,
    changed: Condvar,
}

impl CountingClock {
    fn wait_for_calls(&self, expected: usize) -> bool {
        let calls = self.calls.lock().unwrap();
        let (calls, _) = self
            .changed
            .wait_timeout_while(calls, Duration::from_secs(5), |calls| *calls < expected)
            .unwrap();
        *calls >= expected
    }
}

impl MonotonicClock for CountingClock {
    fn now(&self) -> f64 {
        let mut calls = self.calls.lock().unwrap();
        *calls += 1;
        self.changed.notify_all();
        0.0
    }
}

#[derive(Debug, Default)]
struct LoadGate {
    started: Mutex<bool>,
    started_changed: Condvar,
    released: Mutex<bool>,
    released_changed: Condvar,
}

impl LoadGate {
    fn wait_until_started(&self) -> bool {
        let started = self.started.lock().unwrap();
        let (started, _) = self
            .started_changed
            .wait_timeout_while(started, Duration::from_secs(5), |started| !*started)
            .unwrap();
        *started
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.released_changed.notify_all();
    }
}

struct BlockingRepository {
    snapshot: PlayerTriviaSnapshot,
    load_calls: Mutex<Vec<GuildId>>,
    gate: Arc<LoadGate>,
}

impl PlayerTriviaSnapshotRepository for BlockingRepository {
    fn load_snapshot(&self, guild_id: GuildId) -> Result<PlayerTriviaSnapshot, String> {
        self.load_calls.lock().unwrap().push(guild_id);
        *self.gate.started.lock().unwrap() = true;
        self.gate.started_changed.notify_all();
        let released = self.gate.released.lock().unwrap();
        let (released, result) = self
            .gate
            .released_changed
            .wait_timeout_while(released, Duration::from_secs(5), |released| !*released)
            .unwrap();
        assert!(!result.timed_out() && *released);
        Ok(self.snapshot.clone())
    }

    fn recent_question_keys(
        &self,
        _discord_id: DiscordId,
        _guild_id: GuildId,
        _since: i64,
    ) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
}

fn players(count: i64) -> Vec<PlayerTriviaPlayer> {
    (1..=count)
        .map(|discord_id| PlayerTriviaPlayer {
            discord_id,
            username: format!("P{discord_id:02}"),
            ..PlayerTriviaPlayer::default()
        })
        .collect()
}

fn stable_service(
    repository: Arc<FakeRepository>,
) -> PlayerTriviaService<FakeRepository, StableRandom, ManualClock> {
    PlayerTriviaService::with_dependencies(
        repository,
        StableRandom,
        Arc::new(ManualClock::new(0.0)),
        PLAYER_TRIVIA_SNAPSHOT_CACHE_TTL_SECONDS,
    )
}

fn generate(
    snapshot: PlayerTriviaSnapshot,
    recent: impl IntoIterator<Item = impl Into<String>>,
    count: usize,
    include_spicy: bool,
    current_members: Option<&BTreeSet<DiscordId>>,
) -> (Vec<PlayerTriviaQuestion>, Arc<FakeRepository>) {
    let repository = FakeRepository::with_snapshot(7, snapshot);
    repository.set_recent(recent);
    let service = stable_service(Arc::clone(&repository));
    let questions = service
        .generate_questions(99, 7, current_members, count, include_spicy, 30)
        .unwrap();
    (questions, repository)
}

fn by_key(questions: &[PlayerTriviaQuestion]) -> BTreeMap<&str, &PlayerTriviaQuestion> {
    questions
        .iter()
        .map(|question| (question.key.as_str(), question))
        .collect()
}

fn rich_snapshot() -> PlayerTriviaSnapshot {
    let mut snapshot = PlayerTriviaSnapshot {
        players: players(8),
        ..PlayerTriviaSnapshot::default()
    };
    let mut sequence = 1_i64;
    let value_with_winner = |player_id: i64, winner: i64, scale: i64| {
        (100 - (player_id - winner).rem_euclid(8)) * scale
    };
    for player_id in 1..=8 {
        for index in 0..player_id {
            snapshot.tips.push(TipRow {
                sender_id: player_id,
                recipient_id: player_id % 8 + 1,
                amount: player_id * 3,
            });
            snapshot.wheel_spins.push(WheelSpinRow {
                spin_id: sequence,
                discord_id: player_id,
                spin_time: sequence,
                outcome_code: Some(format!("OUTCOME_{}", index % 4)),
            });
            sequence += 1;
        }
        snapshot.tunnels.push(TunnelRow {
            discord_id: player_id,
            total_digs: value_with_winner(player_id, 2, 10),
            max_depth: value_with_winner(player_id, 3, 9),
            total_jc_earned: value_with_winner(player_id, 4, 100),
            prestige_level: value_with_winner(player_id, 5, 1),
            best_run_score: value_with_winner(player_id, 6, 20),
            ..TunnelRow::default()
        });
        snapshot.bankruptcies.push(BankruptcyRow {
            discord_id: player_id,
            bankruptcy_count: 9 - player_id,
        });
        let trivia_count = 9 - (player_id - 7).rem_euclid(8);
        for index in 0..trivia_count {
            snapshot.trivia_sessions.push(TriviaHistoryRow {
                discord_id: player_id,
                streak: if player_id == 6 { 20 } else { player_id } + index,
                jc_earned: player_id * 2,
            });
            sequence += 1;
        }
    }
    snapshot
}

#[test]
fn test_snapshot_cache_reuses_same_guild_load() {
    let repository = FakeRepository::with_snapshot(
        7,
        PlayerTriviaSnapshot {
            players: players(4),
            ..PlayerTriviaSnapshot::default()
        },
    );
    let service = stable_service(Arc::clone(&repository));

    service
        .generate_questions(1, 7, None, 1, false, 30)
        .unwrap();
    service
        .generate_questions(2, 7, None, 1, false, 30)
        .unwrap();

    assert_eq!(repository.load_calls(), vec![7]);
    assert_eq!(repository.recent_calls().len(), 2);
}

#[test]
fn test_snapshot_cache_expires_using_monotonic_time() {
    let repository = FakeRepository::with_snapshot(
        7,
        PlayerTriviaSnapshot {
            players: players(4),
            ..PlayerTriviaSnapshot::default()
        },
    );
    let clock = Arc::new(ManualClock::new(100.0));
    let service = PlayerTriviaService::with_dependencies(
        Arc::clone(&repository),
        StableRandom,
        Arc::clone(&clock),
        45.0,
    );

    service.snapshot_for_generation(7).unwrap();
    clock.advance(44.999);
    service.snapshot_for_generation(7).unwrap();
    clock.advance(0.001);
    service.snapshot_for_generation(7).unwrap();

    assert_eq!(repository.load_calls(), vec![7, 7]);
}

#[test]
fn test_snapshot_cache_prunes_expired_inactive_guilds() {
    let repository = FakeRepository::with_snapshot(
        7,
        PlayerTriviaSnapshot {
            players: players(4),
            ..PlayerTriviaSnapshot::default()
        },
    );
    repository.set_snapshot(
        8,
        PlayerTriviaSnapshot {
            players: players(4),
            ..PlayerTriviaSnapshot::default()
        },
    );
    let clock = Arc::new(ManualClock::new(100.0));
    let service = PlayerTriviaService::with_dependencies(
        Arc::clone(&repository),
        StableRandom,
        Arc::clone(&clock),
        45.0,
    );

    service.snapshot_for_generation(7).unwrap();
    assert_eq!(service.cached_guild_ids().unwrap(), BTreeSet::from([7]));
    clock.advance(45.0);
    service.snapshot_for_generation(8).unwrap();

    assert_eq!(service.cached_guild_ids().unwrap(), BTreeSet::from([8]));
    assert_eq!(repository.load_calls(), vec![7, 8]);
}

#[test]
fn test_snapshot_cache_is_isolated_by_guild() {
    let mut guild_seven_players = players(4);
    guild_seven_players[0].username = "Guild 7".to_owned();
    let mut guild_eight_players = players(4);
    guild_eight_players[0].username = "Guild 8".to_owned();
    let repository = FakeRepository::with_snapshot(
        7,
        PlayerTriviaSnapshot {
            players: guild_seven_players,
            ..PlayerTriviaSnapshot::default()
        },
    );
    repository.set_snapshot(
        8,
        PlayerTriviaSnapshot {
            players: guild_eight_players,
            ..PlayerTriviaSnapshot::default()
        },
    );
    let service = stable_service(Arc::clone(&repository));

    let seven = service.snapshot_for_generation(7).unwrap();
    let eight = service.snapshot_for_generation(8).unwrap();
    let seven_again = service.snapshot_for_generation(7).unwrap();

    assert_eq!(seven.players[0].username, "Guild 7");
    assert_eq!(eight.players[0].username, "Guild 8");
    assert_eq!(seven_again.players[0].username, "Guild 7");
    assert_eq!(repository.load_calls(), vec![7, 8]);
}

#[test]
fn test_snapshot_cache_collapses_concurrent_same_guild_misses() {
    let worker_count = 8;
    let gate = Arc::new(LoadGate::default());
    let repository = Arc::new(BlockingRepository {
        snapshot: PlayerTriviaSnapshot {
            players: players(4),
            ..PlayerTriviaSnapshot::default()
        },
        load_calls: Mutex::new(Vec::new()),
        gate: Arc::clone(&gate),
    });
    let clock = Arc::new(CountingClock::default());
    let service = Arc::new(PlayerTriviaService::with_dependencies(
        Arc::clone(&repository),
        StableRandom,
        Arc::clone(&clock),
        45.0,
    ));
    let barrier = Arc::new(Barrier::new(worker_count));

    let workers = (0..worker_count)
        .map(|_| {
            let service = Arc::clone(&service);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                service.snapshot_for_generation(7).unwrap()
            })
        })
        .collect::<Vec<_>>();
    assert!(gate.wait_until_started());
    assert!(clock.wait_for_calls(worker_count));
    gate.release();
    let snapshots = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(*repository.load_calls.lock().unwrap(), vec![7]);
    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot.players[0].username == "P01")
    );
    let addresses = snapshots
        .iter()
        .map(|snapshot| snapshot.players.as_ptr())
        .collect::<BTreeSet<_>>();
    assert_eq!(addresses.len(), worker_count);
}

#[test]
fn test_snapshot_cache_returns_isolated_per_call_copies() {
    let repository = FakeRepository::with_snapshot(
        7,
        PlayerTriviaSnapshot {
            players: players(4),
            tips: vec![TipRow {
                sender_id: 1,
                recipient_id: 2,
                amount: 10,
            }],
            ..PlayerTriviaSnapshot::default()
        },
    );
    let service = stable_service(Arc::clone(&repository));

    let mut first = service.snapshot_for_generation(7).unwrap();
    first.players[0].username = "Changed".to_owned();
    first.players.push(PlayerTriviaPlayer {
        discord_id: 99,
        username: "Added".to_owned(),
        ..PlayerTriviaPlayer::default()
    });
    first.tips.clear();
    let second = service.snapshot_for_generation(7).unwrap();
    let cached = service.raw_snapshot(7).unwrap();

    assert_eq!(
        second
            .players
            .iter()
            .map(|player| player.username.as_str())
            .collect::<Vec<_>>(),
        vec!["P01", "P02", "P03", "P04"]
    );
    assert_eq!(second.tips.len(), 1);
    assert_eq!(cached.players[0].username, "P01");
    assert_eq!(repository.load_calls(), vec![7]);
}

#[test]
fn test_snapshot_cache_does_not_cache_failed_loads() {
    let repository = FakeRepository::with_snapshot(
        7,
        PlayerTriviaSnapshot {
            players: players(4),
            ..PlayerTriviaSnapshot::default()
        },
    );
    repository.queue_load(Err("snapshot unavailable".to_owned()));
    repository.queue_load(Ok(PlayerTriviaSnapshot {
        players: players(4),
        ..PlayerTriviaSnapshot::default()
    }));
    let service = stable_service(Arc::clone(&repository));

    assert_eq!(
        service.snapshot_for_generation(7).unwrap_err(),
        PlayerTriviaServiceError::Repository("snapshot unavailable".to_owned())
    );
    assert_eq!(service.snapshot_for_generation(7).unwrap().players.len(), 4);
    assert_eq!(repository.load_calls(), vec![7, 7]);
}

#[test]
fn test_question_record_and_persistence_delegates_use_immutable_snapshot_shape() {
    let question = PlayerTriviaQuestion::new(
        "matches:most_wins",
        "matches",
        "Who?",
        ["A", "B", "C", "D"].map(str::to_owned),
        2,
        "C had 12 wins.",
        true,
    )
    .unwrap();
    assert_eq!(
        question.to_record(),
        PlayerTriviaQuestionRecord {
            question_key: "matches:most_wins".to_owned(),
            category: "matches".to_owned(),
            question_text: "Who?".to_owned(),
            options: ["A", "B", "C", "D"].map(str::to_owned),
            correct_index: 2,
            explanation: "C had 12 wins.".to_owned(),
            spicy: true,
        }
    );
    assert_eq!(
        PlayerTriviaQuestion::new(
            "k",
            "c",
            "t",
            ["A", "A", "B", "C"].map(str::to_owned),
            0,
            "e",
            false,
        )
        .unwrap_err(),
        PlayerTriviaQuestionError::DuplicateOptions
    );

    let repository = FakeRepository::with_snapshot(7, PlayerTriviaSnapshot::default());
    let service = stable_service(Arc::clone(&repository));
    assert_eq!(
        service
            .try_start_session(1, 2, std::slice::from_ref(&question), 100, 300, true)
            .unwrap(),
        4
    );
    assert!(service.settle_answer(4, 2, 1, 25, 101).unwrap());
    service.finish_session(4, "completed", 102).unwrap();
    assert!(service.cancel_session_if_unanswered(4).unwrap());

    assert_eq!(
        repository.session_calls(),
        vec![
            SessionCall::Start {
                discord_id: 1,
                guild_id: 2,
                questions: vec![question.to_record()],
                now: 100,
                cooldown_seconds: 300,
                bypass: true,
            },
            SessionCall::Answer {
                session_id: 4,
                question_number: 2,
                selected_index: 1,
                reward: 25,
                answered_at: 101,
            },
            SessionCall::Finish {
                session_id: 4,
                status: "completed".to_owned(),
                completed_at: 102,
            },
            SessionCall::Cancel { session_id: 4 },
        ]
    );
}

#[test]
fn test_generation_is_balanced_four_option_recent_aware_and_deterministic() {
    let first_repository = FakeRepository::with_snapshot(7, rich_snapshot());
    let second_repository = FakeRepository::with_snapshot(7, rich_snapshot());
    let first_service = PlayerTriviaService::with_dependencies(
        first_repository,
        SeededTriviaRandom::new(144),
        Arc::new(ManualClock::new(0.0)),
        45.0,
    );
    let second_service = PlayerTriviaService::with_dependencies(
        second_repository,
        SeededTriviaRandom::new(144),
        Arc::new(ManualClock::new(0.0)),
        45.0,
    );
    let first = first_service
        .generate_questions(99, 7, None, 10, false, 30)
        .unwrap();
    let second = second_service
        .generate_questions(99, 7, None, 10, false, 30)
        .unwrap();

    assert_eq!(first, second);
    assert!(first.len() >= 7);
    assert!(first.iter().all(|question| {
        question
            .options
            .iter()
            .map(|option| option.to_lowercase())
            .collect::<BTreeSet<_>>()
            .len()
            == 4
    }));
    assert!(first.iter().all(|question| !question.spicy));
    let mut categories = BTreeMap::<&str, usize>::new();
    for question in &first {
        *categories.entry(&question.category).or_default() += 1;
    }
    assert!(categories.values().all(|count| *count <= 2));

    let hidden = first[0].key.clone();
    let recent_repository = FakeRepository::with_snapshot(7, rich_snapshot());
    recent_repository.set_recent([hidden.clone()]);
    let recent_service = PlayerTriviaService::with_dependencies(
        Arc::clone(&recent_repository),
        SeededTriviaRandom::new(144),
        Arc::new(ManualClock::new(0.0)),
        45.0,
    );
    let without_recent = recent_service
        .generate_questions(99, 7, None, 20, false, 30)
        .unwrap();
    assert!(without_recent.iter().all(|question| question.key != hidden));
    let recent_call = recent_repository.recent_calls()[0];
    assert_eq!((recent_call.0, recent_call.1), (99, 7));
}

#[test]
fn test_current_member_filter_removes_departed_players_before_aggregation() {
    let snapshot = PlayerTriviaSnapshot {
        players: players(8),
        tips: (1..=8)
            .map(|player_id| TipRow {
                sender_id: player_id,
                recipient_id: 1,
                amount: player_id * 10,
            })
            .collect(),
        ..PlayerTriviaSnapshot::default()
    };
    let members = (1..=7).collect::<BTreeSet<_>>();
    let (questions, _) = generate(snapshot, [] as [&str; 0], 50, false, Some(&members));
    let question = by_key(&questions)["tips:most_jc_sent"];

    assert_eq!(question.correct_answer(), "<@7>");
    let rendered = questions
        .iter()
        .flat_map(|question| {
            std::iter::once(question.text.as_str())
                .chain(std::iter::once(question.explanation.as_str()))
                .chain(question.options.iter().map(String::as_str))
        })
        .collect::<Vec<_>>()
        .join(" ");
    assert!(!rendered.contains("<@8>"));
}

#[test]
fn test_displayed_win_rate_ties_are_rejected() {
    let mut snapshot_players = players(4);
    for (player, (wins, losses)) in
        snapshot_players
            .iter_mut()
            .zip([(91, 9), (909, 90), (80, 20), (70, 30)])
    {
        player.wins = wins;
        player.losses = losses;
    }
    let (questions, _) = generate(
        PlayerTriviaSnapshot {
            players: snapshot_players,
            ..PlayerTriviaSnapshot::default()
        },
        [] as [&str; 0],
        50,
        false,
        None,
    );
    assert!(!by_key(&questions).contains_key("matches:highest_win_rate"));
}

#[test]
fn test_glicko_openskill_display_and_rank_discrepancies_use_same_scale() {
    let mut snapshot_players = players(4);
    for (player, (glicko, mu)) in snapshot_players.iter_mut().zip([
        (2_500.0, 25.0),
        (2_000.0, 55.0),
        (1_500.0, 53.0),
        (1_000.0, 43.0),
    ]) {
        player.wins = 20;
        player.losses = 10;
        player.glicko_rating = Some(glicko);
        player.glicko_rd = Some(50.0);
        player.openskill_mu = Some(mu);
        player.openskill_sigma = Some(2.0);
    }
    let snapshot = PlayerTriviaSnapshot {
        players: snapshot_players,
        ..PlayerTriviaSnapshot::default()
    };
    let (display_questions, _) = generate(
        snapshot.clone(),
        ["ratings:glicko_leader", "ratings:openskill_leader"],
        50,
        false,
        None,
    );
    let display = by_key(&display_questions)["ratings:largest_display_gap"];
    assert_eq!(display.correct_answer(), "<@1>");
    assert!(display.explanation.contains("2500 points"));

    let (rank_questions, _) = generate(
        snapshot,
        [
            "ratings:glicko_leader",
            "ratings:openskill_leader",
            "ratings:largest_display_gap",
            "ratings:smallest_display_gap",
        ],
        50,
        false,
        None,
    );
    let rank = by_key(&rank_questions)["ratings:largest_rank_gap"];
    assert_eq!(rank.correct_answer(), "<@1>");
    assert!(rank.explanation.contains("3 places"));
}

#[test]
fn test_spicy_questions_are_opt_in_but_neutral_loss_history_is_not_spicy() {
    let mut snapshot_players = players(4);
    for (player, (balance, lowest)) in
        snapshot_players
            .iter_mut()
            .zip([(100, -10), (200, -20), (300, -30), (-500, -900)])
    {
        player.balance = balance;
        player.lowest_balance_ever = Some(lowest);
    }
    let economy_snapshot = PlayerTriviaSnapshot {
        players: snapshot_players,
        ..PlayerTriviaSnapshot::default()
    };
    let (normal, _) = generate(economy_snapshot.clone(), [] as [&str; 0], 50, false, None);
    let (spicy, _) = generate(economy_snapshot.clone(), [] as [&str; 0], 50, true, None);
    let (deep_spicy, _) = generate(
        economy_snapshot,
        ["economy:highest_balance", "economy:lowest_balance"],
        50,
        true,
        None,
    );
    assert!(!by_key(&normal).contains_key("economy:lowest_balance"));
    assert!(by_key(&spicy)["economy:lowest_balance"].spicy);
    assert!(!by_key(&normal).contains_key("economy:deepest_historical_debt"));
    assert_eq!(
        by_key(&deep_spicy)["economy:deepest_historical_debt"].correct_answer(),
        "<@4>"
    );

    let hero_sets = BTreeMap::from([
        (1, vec![1, 2, 3, 4, 5]),
        (2, vec![6, 7, 8, 9]),
        (3, vec![10, 11, 12]),
        (4, vec![13, 13, 14]),
    ]);
    let mut match_id = 1;
    let mut participants = Vec::new();
    for (player_id, heroes) in hero_sets {
        for hero_id in heroes {
            participants.push(ParticipantRow {
                discord_id: player_id,
                match_id,
                match_time: match_id,
                hero_id,
                won: false,
                ..ParticipantRow::default()
            });
            match_id += 1;
        }
    }
    let (hero_questions, _) = generate(
        PlayerTriviaSnapshot {
            players: players(4),
            participants,
            ..PlayerTriviaSnapshot::default()
        },
        [] as [&str; 0],
        50,
        false,
        None,
    );
    let lost = by_key(&hero_questions)["heroes:most_distinct_lost"];
    assert_eq!(lost.correct_answer(), "<@1>");
    assert!(!lost.spicy);
}

#[test]
fn test_tip_amount_and_transaction_leaders_are_separate_statistics() {
    let mut tips = vec![TipRow {
        sender_id: 1,
        recipient_id: 6,
        amount: 1_000,
    }];
    for (player_id, count, amount) in [(2, 5, 10), (3, 4, 5), (4, 3, 2), (5, 2, 1)] {
        tips.extend((0..count).map(|_| TipRow {
            sender_id: player_id,
            recipient_id: 6,
            amount,
        }));
    }
    let (questions, _) = generate(
        PlayerTriviaSnapshot {
            players: players(6),
            tips,
            ..PlayerTriviaSnapshot::default()
        },
        [] as [&str; 0],
        50,
        false,
        None,
    );
    let keyed = by_key(&questions);
    assert_eq!(keyed["tips:most_jc_sent"].correct_answer(), "<@1>");
    assert_eq!(
        keyed["tips:most_transactions_sent"].correct_answer(),
        "<@2>"
    );
}

#[test]
fn test_exact_wheel_outcome_supports_lightning_frequency_and_ignores_null_codes() {
    let mut rows = Vec::new();
    let mut spin_id = 1;
    for (player_id, count) in [(1, 100), (2, 4), (3, 2), (4, 1)] {
        for _ in 0..count {
            rows.push(WheelSpinRow {
                spin_id,
                discord_id: player_id,
                spin_time: spin_id,
                outcome_code: Some("LIGHTNING".to_owned()),
            });
            spin_id += 1;
        }
    }
    for _ in 0..200 {
        rows.push(WheelSpinRow {
            spin_id,
            discord_id: 6,
            spin_time: spin_id,
            outcome_code: None,
        });
        spin_id += 1;
    }
    let (questions, _) = generate(
        PlayerTriviaSnapshot {
            players: players(6),
            wheel_spins: rows,
            ..PlayerTriviaSnapshot::default()
        },
        [
            "wheel:most_spins",
            "wheel:most_common_outcome:1",
            "wheel:most_common_outcome:2",
        ],
        50,
        false,
        None,
    );
    let keyed = by_key(&questions);
    let leader = keyed["wheel:exact_outcome_leader:lightning"];
    let count = keyed["wheel:exact_outcome_count:lightning:1"];
    assert_eq!(leader.correct_answer(), "<@1>");
    assert_eq!(count.correct_answer(), "100 times");
    assert!(count.text.contains("Lightning"));
    assert!(
        questions
            .iter()
            .all(|question| !question.key.to_lowercase().contains("none"))
    );
}

#[test]
fn test_finalized_disbursement_history_can_ask_about_burn() {
    let mut rows = Vec::new();
    let mut proposal_id = 1;
    for _ in 0..3 {
        rows.push(DisbursementVoteRow {
            proposal_id,
            discord_id: 1,
            vote_method: "burn".to_owned(),
            proposal_outcome: "burn".to_owned(),
            finalized_at: Some(proposal_id),
        });
        proposal_id += 1;
    }
    for (player_id, method) in [
        (2, "even"),
        (3, "proportional"),
        (4, "neediest"),
        (5, "stimulus"),
    ] {
        rows.push(DisbursementVoteRow {
            proposal_id,
            discord_id: player_id,
            vote_method: method.to_owned(),
            proposal_outcome: method.to_owned(),
            finalized_at: Some(proposal_id),
        });
        proposal_id += 1;
    }
    let (questions, _) = generate(
        PlayerTriviaSnapshot {
            players: players(5),
            disbursement_votes: rows,
            ..PlayerTriviaSnapshot::default()
        },
        ["disburse:most_finalized_ballots"],
        50,
        false,
        None,
    );
    let question = by_key(&questions)["disburse:most_common_vote:1"];
    assert_eq!(question.correct_answer(), "Burn");
    assert!(question.text.to_lowercase().contains("finalized"));
}

fn prediction_position(
    player_id: DiscordId,
    prediction_id: i64,
    profitable: bool,
) -> PredictionPositionRow {
    PredictionPositionRow {
        prediction_id,
        discord_id: player_id,
        status: "resolved".to_owned(),
        outcome: "yes".to_owned(),
        yes_contracts: 1,
        yes_cost_basis_total: if profitable { 0 } else { 20 },
        question: Some(format!("Will market {prediction_id} resolve YES?")),
        ..PredictionPositionRow::default()
    }
}

#[test]
fn test_prediction_questions_include_market_descriptor_without_creator_metadata() {
    let signs = BTreeMap::from([
        (1, vec![false, true, true]),
        (2, vec![true, true, true]),
        (3, vec![false, false, true]),
        (4, vec![false, false, false]),
    ]);
    let positions = signs
        .into_iter()
        .flat_map(|(player_id, signs)| {
            signs.into_iter().enumerate().map(move |(offset, sign)| {
                prediction_position(player_id, player_id * 10 + offset as i64 + 1, sign)
            })
        })
        .collect();
    let (questions, _) = generate(
        PlayerTriviaSnapshot {
            players: players(6),
            prediction_positions: positions,
            ..PlayerTriviaSnapshot::default()
        },
        ["predictions:most_wins", "predictions:best_total_pnl"],
        50,
        false,
        None,
    );
    let loss = by_key(&questions)["predictions:loss_market:1:11"];
    assert_eq!(
        loss.correct_answer(),
        "Market #11 — Will market 11 resolve YES?"
    );
    assert!(loss.text.contains("<@1>"));
    assert!(!loss.spicy);
    let rendered = questions
        .iter()
        .flat_map(|question| {
            std::iter::once(question.text.as_str())
                .chain(std::iter::once(question.explanation.as_str()))
                .chain(question.options.iter().map(String::as_str))
        })
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    assert!(!rendered.contains("creator"));
    assert!(!rendered.contains("secret meta"));
}

#[test]
fn test_prediction_questions_subtract_vanity_tax_from_total_pnl() {
    let mut positions = Vec::new();
    for player_id in 1..=6 {
        for offset in 0..3 {
            positions.push(PredictionPositionRow {
                prediction_id: player_id * 10 + offset,
                discord_id: player_id,
                status: "resolved".to_owned(),
                outcome: "yes".to_owned(),
                yes_contracts: 1,
                yes_cost_basis_total: if player_id == 1 { 0 } else { player_id },
                vanity_tax: if player_id == 1 { 5 } else { 0 },
                low_priority_tax: 0,
                ..PredictionPositionRow::default()
            });
        }
    }
    let (questions, _) = generate(
        PlayerTriviaSnapshot {
            players: players(6),
            prediction_positions: positions,
            ..PlayerTriviaSnapshot::default()
        },
        [] as [&str; 0],
        50,
        false,
        None,
    );
    let question = by_key(&questions)["predictions:best_total_pnl"];
    assert_eq!(question.correct_answer(), "<@2>");
    assert!(question.explanation.contains("+24 JC"));
}

#[test]
fn test_betting_questions_subtract_vanity_tax_once_per_settlement() {
    let mut rows = vec![
        SettledBetRow {
            bet_id: 1,
            discord_id: 1,
            match_id: 11,
            team_bet_on: "radiant".to_owned(),
            winning_team: 1,
            amount: 50,
            leverage: 1,
            effective_bet: Some(50),
            payout: 100,
            settlement_vanity_tax: 1,
            settlement_low_priority_tax: 0,
        },
        SettledBetRow {
            bet_id: 2,
            discord_id: 1,
            match_id: 11,
            team_bet_on: "radiant".to_owned(),
            winning_team: 1,
            amount: 50,
            leverage: 1,
            effective_bet: Some(50),
            payout: 100,
            settlement_vanity_tax: 1,
            settlement_low_priority_tax: 0,
        },
    ];
    for (index, amount) in [34, 33, 33].into_iter().enumerate() {
        rows.push(SettledBetRow {
            bet_id: index as i64 + 3,
            discord_id: 1,
            match_id: index as i64 + 12,
            team_bet_on: "dire".to_owned(),
            winning_team: 1,
            amount,
            leverage: 1,
            effective_bet: Some(amount),
            payout: 0,
            settlement_vanity_tax: 0,
            settlement_low_priority_tax: 0,
        });
    }
    let mut next_bet_id = 10;
    for player_id in 2..=6 {
        for offset in 0..5 {
            rows.push(SettledBetRow {
                bet_id: next_bet_id,
                discord_id: player_id,
                match_id: player_id * 100 + offset,
                team_bet_on: "radiant".to_owned(),
                winning_team: 1,
                amount: 1,
                leverage: 1,
                effective_bet: Some(1),
                payout: 1,
                settlement_vanity_tax: 0,
                settlement_low_priority_tax: 0,
            });
            next_bet_id += 1;
        }
    }
    let (questions, _) = generate(
        PlayerTriviaSnapshot {
            players: players(6),
            bets: rows,
            ..PlayerTriviaSnapshot::default()
        },
        [] as [&str; 0],
        50,
        true,
        None,
    );
    let question = by_key(&questions)["betting:largest_loss"];
    assert_eq!(question.correct_answer(), "<@1>");
    assert!(question.explanation.contains("down 1 JC"));
}

#[test]
fn test_mafia_questions_use_only_resolved_non_cancelled_games() {
    let mut games = (1..=5)
        .map(|game_id| MafiaGameRow {
            game_id,
            phase: "RESOLVED".to_owned(),
            status: "COMPLETED".to_owned(),
            winner: Some("TOWN".to_owned()),
            mvp_id: (game_id == 1).then_some(1),
        })
        .collect::<Vec<_>>();
    let mut mafia_players = Vec::new();
    for game_id in 1..=5 {
        for (player_id, games_played) in [(1, 5), (2, 4), (3, 3), (4, 2)] {
            if game_id <= games_played {
                mafia_players.push(MafiaPlayerRow {
                    game_id,
                    discord_id: player_id,
                    role: "TOWNIE".to_owned(),
                });
            }
        }
    }
    for game_id in 100..120 {
        games.push(MafiaGameRow {
            game_id,
            phase: "NIGHT".to_owned(),
            status: "ACTIVE".to_owned(),
            winner: None,
            mvp_id: None,
        });
        mafia_players.push(MafiaPlayerRow {
            game_id,
            discord_id: 5,
            role: "MAFIA".to_owned(),
        });
    }
    games.push(MafiaGameRow {
        game_id: 200,
        phase: "RESOLVED".to_owned(),
        status: "CANCELLED".to_owned(),
        winner: Some("TOWN".to_owned()),
        mvp_id: None,
    });
    mafia_players.push(MafiaPlayerRow {
        game_id: 200,
        discord_id: 6,
        role: "TOWNIE".to_owned(),
    });

    let (questions, _) = generate(
        PlayerTriviaSnapshot {
            players: players(6),
            mafia_games: games,
            mafia_players,
            ..PlayerTriviaSnapshot::default()
        },
        [] as [&str; 0],
        50,
        false,
        None,
    );
    let question = by_key(&questions)["mafia:most_games"];
    assert_eq!(question.correct_answer(), "<@1>");
    assert!(question.text.to_lowercase().contains("resolved"));
}

#[test]
fn every_live_python_candidate_family_is_representable_from_typed_snapshot() {
    let mut snapshot = PlayerTriviaSnapshot {
        players: (1_i64..=8)
            .map(|player_id| PlayerTriviaPlayer {
                discord_id: player_id,
                username: format!("Player {player_id}"),
                wins: 100 - player_id * 4,
                losses: player_id * 3,
                balance: player_id * 100,
                lowest_balance_ever: Some(-player_id * 100),
                glicko_rating: Some(2_100.0 - player_id as f64 * 70.0),
                glicko_rd: Some(80.0),
                openskill_mu: Some(match player_id {
                    1 => 20.0,
                    2 => 49.0,
                    3 => 50.0,
                    _ => 52.0 - player_id as f64,
                }),
                openskill_sigma: Some(3.0),
                last_match_unix: None,
            })
            .collect(),
        ..PlayerTriviaSnapshot::default()
    };

    let mut match_id = 1_i64;
    for player_id in 1_i64..=8 {
        for index in 0_i64..18 {
            let hero_offset = match index {
                0..=5 => 0,
                6..=9 => 1,
                10..=13 => 2,
                _ => 3,
            };
            let base_win = match hero_offset {
                0 => true,
                1 => index <= 8,
                2 => index <= 11,
                _ => false,
            };
            let won = if index >= 14 {
                index >= 18 - (player_id % 5)
            } else {
                base_win
            };
            snapshot.participants.push(ParticipantRow {
                discord_id: player_id,
                match_id,
                match_time: match_id,
                hero_id: player_id * 10 + hero_offset,
                won,
                kills: Some((player_id * 10 + index) as f64),
                assists: Some((player_id * 20 + index) as f64),
                gpm: Some((player_id * 100 + index) as f64),
                hero_damage: Some((player_id * 1_000 + index) as f64),
                tower_damage: Some((player_id * 500 + index) as f64),
                fantasy_points: Some((player_id * 30 + index) as f64),
                lane_role: Some(if index < 10 { player_id % 5 } else { index % 5 }),
            });
            match_id += 1;
        }
        for index in 0_i64..player_id * 2 {
            snapshot.participants.push(ParticipantRow {
                discord_id: player_id,
                match_id,
                match_time: match_id,
                hero_id: player_id * 1_000 + index,
                won: index >= player_id,
                kills: Some((player_id * 10 + index) as f64),
                assists: Some((player_id * 20 + index) as f64),
                gpm: Some((player_id * 100 + index) as f64),
                hero_damage: Some((player_id * 1_000 + index) as f64),
                tower_damage: Some((player_id * 500 + index) as f64),
                fantasy_points: Some((player_id * 30 + index) as f64),
                lane_role: Some(player_id % 5),
            });
            match_id += 1;
        }
        snapshot.ratings.push(RatingRow {
            discord_id: player_id,
            rating: Some(1_500.0 + player_id as f64 * 20.0),
            rating_before: Some(1_500.0 + player_id as f64 * 10.0),
        });
        snapshot.recalibrations.push(RecalibrationRow {
            discord_id: player_id,
            total_recalibrations: player_id,
        });
        snapshot.bankruptcies.push(BankruptcyRow {
            discord_id: player_id,
            bankruptcy_count: player_id,
        });
        snapshot.tunnels.push(TunnelRow {
            discord_id: player_id,
            total_digs: player_id * 20,
            max_depth: player_id * 30,
            total_jc_earned: player_id * 100,
            prestige_level: player_id,
            best_run_score: player_id * 1_000,
            pickaxe_tier: player_id,
            stat_strength: player_id * 2,
            stat_smarts: player_id * 3,
            stat_stamina: player_id * 4,
        });
        for index in 0_i64..player_id + 4 {
            snapshot.tips.push(TipRow {
                sender_id: player_id,
                recipient_id: player_id % 8 + 1,
                amount: player_id * 10,
            });
            snapshot.bets.push(SettledBetRow {
                bet_id: player_id * 100 + index,
                discord_id: player_id,
                match_id: player_id * 100 + index,
                team_bet_on: "radiant".to_owned(),
                winning_team: if index < player_id { 1 } else { 2 },
                amount: player_id * 10,
                leverage: 1,
                effective_bet: Some(player_id * 10),
                payout: if index < player_id { player_id * 25 } else { 0 },
                settlement_vanity_tax: player_id,
                settlement_low_priority_tax: 0,
            });
            snapshot.wheel_spins.push(WheelSpinRow {
                spin_id: player_id * 100 + index,
                discord_id: player_id,
                spin_time: player_id * 100 + index,
                outcome_code: Some(
                    ["LIGHTNING", "GOLDEN", "BANKRUPT", "JACKPOT"]
                        [usize::try_from(index % 4).expect("wheel index")]
                    .to_owned(),
                ),
            });
            snapshot.double_spins.push(DoubleSpinRow {
                discord_id: player_id,
                won: index < player_id,
            });
            snapshot.trivia_sessions.push(TriviaHistoryRow {
                discord_id: player_id,
                streak: player_id + index,
                jc_earned: player_id * 3,
            });
        }
        for _index in 0_i64..player_id {
            snapshot.dig_artifacts.push(DigArtifactRow {
                discord_id: player_id,
                is_relic: true,
            });
            snapshot.dig_achievements.push(DigAchievementRow {
                discord_id: player_id,
            });
            snapshot.dig_actions.push(DigActionRow {
                actor_id: player_id,
                action_type: "cave_in".to_owned(),
                depth_before: Some(10),
                depth_after: Some(5),
            });
        }
    }

    for other_id in 2_i64..=8 {
        snapshot.pairings.push(PairingRow {
            player1_id: 1,
            player2_id: other_id,
            games_together: 30 - other_id,
            wins_together: 30 - other_id * 2,
            games_against: 25 - other_id,
            player1_wins_against: other_id,
        });
    }

    let mut prediction_id = 1_i64;
    for player_id in 1_i64..=8 {
        for index in 0_i64..10 {
            let contracts = player_id + index + 1;
            let payout = contracts * CONTRACT_VALUE;
            let profit = index < player_id;
            snapshot.prediction_positions.push(PredictionPositionRow {
                prediction_id,
                discord_id: player_id,
                status: "resolved".to_owned(),
                outcome: "yes".to_owned(),
                yes_contracts: contracts,
                yes_cost_basis_total: if profit {
                    payout.saturating_sub(player_id * 10)
                } else {
                    payout.saturating_add(player_id * 10)
                },
                no_contracts: 0,
                no_cost_basis_total: 0,
                bankruptcy_penalty: 0,
                vanity_tax: 0,
                low_priority_tax: 0,
                question: Some(format!("Will market {prediction_id} resolve yes?")),
            });
            for _ in 0..player_id + 2 {
                snapshot.prediction_trades.push(PredictionTradeRow {
                    prediction_id,
                    discord_id: player_id,
                    contracts: player_id * 3,
                    status: "resolved".to_owned(),
                });
            }
            prediction_id += 1;
        }
    }

    let roles = [
        "TOWNIE",
        "DOCTOR",
        "DETECTIVE",
        "VIGILANTE",
        "MAFIA",
        "JESTER",
        "TOWNIE",
        "MAFIA",
    ];
    let mut mvp_sequence = Vec::new();
    for player_id in 1_i64..=8 {
        mvp_sequence.extend(std::iter::repeat_n(player_id, player_id as usize));
    }
    for game_id in 1_i64..=36 {
        let winner = if game_id % 2 == 0 { "TOWN" } else { "MAFIA" };
        snapshot.mafia_games.push(MafiaGameRow {
            game_id,
            phase: "RESOLVED".to_owned(),
            status: "COMPLETED".to_owned(),
            winner: Some(winner.to_owned()),
            mvp_id: Some(mvp_sequence[usize::try_from(game_id - 1).expect("MVP index")]),
        });
        for player_id in 1_i64..=8 {
            if game_id > 20 + player_id * 2 {
                continue;
            }
            snapshot.mafia_players.push(MafiaPlayerRow {
                game_id,
                discord_id: player_id,
                role: roles[usize::try_from(player_id - 1).expect("role index")].to_owned(),
            });
        }
    }
    for (action_type, _) in [
        ("KILL", "Mafia kill choices"),
        ("VIG_KILL", "Vigilante kill choices"),
        ("SAVE", "Doctor save choices"),
        ("INVESTIGATE", "Detective investigations"),
        ("VOTE", "Mafia votes"),
    ] {
        for player_id in 1_i64..=8 {
            for _ in 0..player_id {
                snapshot.mafia_actions.push(MafiaActionRow {
                    game_id: 1,
                    actor_id: player_id,
                    action_type: action_type.to_owned(),
                });
            }
        }
    }

    let methods = ["burn", "equal", "lottery", "weighted"];
    let outcomes = ["burned", "paid", "rolled", "returned"];
    for player_id in 1_i64..=8 {
        for index in 0_i64..player_id + 4 {
            snapshot.disbursement_votes.push(DisbursementVoteRow {
                proposal_id: player_id * 100 + index,
                discord_id: player_id,
                vote_method: if index < player_id + 1 {
                    "burn".to_owned()
                } else {
                    methods[usize::try_from(index % 4).expect("method index")].to_owned()
                },
                proposal_outcome: outcomes[usize::try_from(index % 4).expect("outcome index")]
                    .to_owned(),
                finalized_at: Some(1_800_000_000 + index),
            });
        }
    }

    let player_index = snapshot
        .players
        .iter()
        .map(|player| (player.discord_id, player))
        .collect::<BTreeMap<_, _>>();
    let mut candidates = Vec::new();
    let mut random = StableRandom;
    build_match_questions(&mut candidates, &snapshot, &player_index, &mut random);
    build_rating_questions(&mut candidates, &snapshot, &player_index, &mut random);
    build_hero_questions(&mut candidates, &snapshot, &player_index, &mut random);
    build_pairing_questions(&mut candidates, &snapshot, &player_index, &mut random);
    build_economy_and_tip_questions(&mut candidates, &snapshot, &player_index, &mut random);
    build_betting_questions(&mut candidates, &snapshot, &player_index, &mut random);
    build_wheel_questions(&mut candidates, &snapshot, &player_index, &mut random);
    build_double_questions(&mut candidates, &snapshot, &player_index, &mut random);
    build_prediction_questions(&mut candidates, &snapshot, &player_index, &mut random);
    build_dig_questions(&mut candidates, &snapshot, &player_index, &mut random);
    build_mafia_questions(&mut candidates, &snapshot, &player_index, &mut random);
    build_trivia_history_questions(&mut candidates, &snapshot, &player_index, &mut random);
    build_disbursement_questions(&mut candidates, &snapshot, &player_index, &mut random);

    let keys = candidates
        .iter()
        .map(|candidate| candidate.question.key.as_str())
        .collect::<BTreeSet<_>>();
    let exact = [
        "matches:most_games",
        "matches:most_wins",
        "matches:highest_win_rate",
        "matches:longest_current_win_streak",
        "matches:lowest_win_rate",
        "matches:most_losses",
        "ratings:glicko_leader",
        "ratings:openskill_leader",
        "ratings:largest_display_gap",
        "ratings:smallest_display_gap",
        "ratings:largest_rank_gap",
        "ratings:smallest_rank_gap",
        "ratings:most_recalibrations",
        "ratings:largest_single_gain",
        "heroes:most_distinct_played",
        "heroes:most_distinct_won",
        "heroes:most_distinct_lost",
        "economy:highest_balance",
        "economy:lowest_balance",
        "economy:deepest_historical_debt",
        "economy:most_bankruptcies",
        "tips:most_jc_sent",
        "tips:most_transactions_sent",
        "tips:most_transactions_received",
        "tips:most_jc_received",
        "betting:most_bets",
        "betting:most_wagered",
        "betting:highest_win_rate",
        "betting:largest_loss",
        "wheel:most_spins",
        "double:most_attempts",
        "double:most_wins",
        "double:highest_win_rate",
        "predictions:most_wins",
        "predictions:best_total_pnl",
        "predictions:most_losses",
        "predictions:most_trades",
        "predictions:most_contract_volume",
        "dig:most_digs",
        "dig:deepest_tunnel",
        "dig:most_jc_earned",
        "dig:highest_prestige",
        "dig:best_run_score",
        "dig:most_artifacts",
        "dig:most_relics",
        "dig:most_achievements",
        "dig:most_cave_ins",
        "mafia:most_games",
        "mafia:most_wins",
        "mafia:most_mvp_awards",
        "trivia:most_sessions",
        "trivia:most_jc_earned",
        "trivia:best_streak",
        "disburse:most_finalized_ballots",
        "disburse:most_common_final_outcome",
    ];
    for key in exact {
        assert!(keys.contains(key), "missing live Python candidate {key}");
    }
    for prefix in [
        "matches:record:",
        "ratings:glicko_rank:",
        "heroes:most_played:",
        "heroes:most_recent:",
        "heroes:most_wins:",
        "heroes:worst_win_rate:",
        "lanes:most_common:",
        "performance:highest_",
        "performance:record_",
        "pairings:most_common_teammate:",
        "pairings:best_teammate:",
        "pairings:worst_teammate:",
        "pairings:most_common_opponent:",
        "pairings:best_opponent:",
        "economy:balance:",
        "wheel:most_common_outcome:",
        "wheel:exact_outcome_leader:",
        "wheel:exact_outcome_count:",
        "wheel:latest_exact_outcome:",
        "predictions:loss_market:",
        "predictions:win_market:",
        "dig:profile:",
        "mafia:most_common_role:",
        "mafia:most_kill_actions",
        "mafia:most_vig_kill_actions",
        "mafia:most_save_actions",
        "mafia:most_investigate_actions",
        "mafia:most_vote_actions",
        "disburse:most_common_vote:",
    ] {
        assert!(
            keys.iter().any(|key| key.starts_with(prefix)),
            "missing live Python candidate family {prefix}"
        );
    }
}

#[test]
fn seeded_trivia_random_is_not_patterned_in_its_low_bits() {
    // A power-of-two LCG's low bits are highly regular: taking state % 2 makes
    // the parity alternate on every call, which biases the shuffled position of
    // the correct answer.
    let mut random = SeededTriviaRandom::new(0x5EED_1234_ABCD_9876);
    let flips = (0..64).map(|_| random.next_index(2)).collect::<Vec<_>>();
    assert!(
        flips.windows(2).any(|pair| pair[0] == pair[1]),
        "a strictly alternating sequence means the low bits are still in use"
    );

    // Every index of a four-way shuffle stays reachable and roughly balanced.
    let mut random = SeededTriviaRandom::new(7);
    let mut counts = [0_usize; 4];
    for _ in 0..4_000 {
        counts[random.next_index(4)] += 1;
    }
    assert!(
        counts.iter().all(|count| *count > 700),
        "all four positions must be reachable, got {counts:?}"
    );
}
