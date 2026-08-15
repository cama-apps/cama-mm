use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use cama_db::schema_manager::initialize_or_migrate;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use crate::admin_provider::{CorrectionWinRewardRequest, CorrectionWinRewardResult};
use crate::gateway_events::{GatewayMember, GuildMemberPageSource};

use super::*;
use cama_domain::rating::CamaRatingSystem;

const GUILD: i64 = 42_424;
const ADMIN: i64 = 7_777;
const WIN_REWARD: i64 = 15;
const MATCH_AT: i64 = 1_786_051_200;

struct RepositoryReward {
    repository: MatchCorrectionRepository,
    calls: AtomicUsize,
}

impl CorrectionWinRewardControl for RepositoryReward {
    fn award_match_win_bonus(
        &self,
        request: CorrectionWinRewardRequest,
    ) -> Result<CorrectionWinRewardResult, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.repository
            .credit_resolved_win_bonus(
                request.match_id,
                Some(request.guild_id),
                request.discord_id,
                request.gross_reward,
            )
            .map_err(|error| error.to_string())?;
        Ok(CorrectionWinRewardResult {
            snapshot_balance_delta: request.gross_reward,
        })
    }
}

struct CompositeRepositoryReward {
    repository: MatchCorrectionRepository,
    composite_player: i64,
    composite_delta: i64,
    calls: AtomicUsize,
}

impl CorrectionWinRewardControl for CompositeRepositoryReward {
    fn award_match_win_bonus(
        &self,
        request: CorrectionWinRewardRequest,
    ) -> Result<CorrectionWinRewardResult, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let delta = if request.discord_id == self.composite_player {
            self.composite_delta
        } else {
            request.gross_reward
        };
        self.repository
            .credit_resolved_win_bonus(
                request.match_id,
                Some(request.guild_id),
                request.discord_id,
                delta,
            )
            .map_err(|error| error.to_string())?;
        Ok(CorrectionWinRewardResult {
            snapshot_balance_delta: delta,
        })
    }
}

struct NoVanityTax;

impl CorrectionVanityTaxSource for NoVanityTax {
    fn taxable_ids(&self, _guild_id: i64) -> BTreeSet<i64> {
        BTreeSet::new()
    }
}

struct FailFirstReplay(AtomicBool);

impl ReplayGate for FailFirstReplay {
    fn before_replay(&self, _guild_id: i64, _match_id: i64) -> Result<(), String> {
        if self.0.swap(false, Ordering::SeqCst) {
            Err("injected replay failure".to_owned())
        } else {
            Ok(())
        }
    }
}

struct EmptyMemberSource;

#[async_trait]
impl GuildMemberPageSource for EmptyMemberSource {
    async fn fetch_page(
        &self,
        _guild_id: u64,
        _after: Option<u64>,
        _limit: u64,
    ) -> Result<Vec<GatewayMember>, String> {
        Ok(Vec::new())
    }
}

struct Fixture {
    database: NamedTempFile,
    match_id: i64,
    radiant: Vec<i64>,
    dire: Vec<i64>,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary correction database");
        initialize_or_migrate(database.path()).expect("initialize Python-compatible schema");
        let connection = Connection::open(database.path()).expect("open correction database");
        // The migrated Python schema intentionally retains a few historical
        // foreign-key declarations that target the pre-guild player key.
        // Production runtime connections make the same compatibility choice.
        connection
            .execute_batch("PRAGMA foreign_keys=OFF")
            .expect("use Python-compatible foreign-key mode");
        let radiant = (1..=5).map(|value| 10_000 + value).collect::<Vec<_>>();
        let dire = (1..=5).map(|value| 20_000 + value).collect::<Vec<_>>();
        let radiant_json = serde_json::to_string(&radiant).expect("radiant JSON");
        let dire_json = serde_json::to_string(&dire).expect("dire JSON");
        connection
            .execute(
                "INSERT INTO matches (
                     team1_players,team2_players,winning_team,match_date,
                     guild_id,win_reward_jc,betting_mode
                 ) VALUES (?1,?2,1,datetime(?3,'unixepoch'),?4,?5,'pool')",
                params![radiant_json, dire_json, MATCH_AT, GUILD, WIN_REWARD],
            )
            .expect("seed recorded match");
        let match_id = connection.last_insert_rowid();

        for (team, ids) in [(MatchSide::Radiant, &radiant), (MatchSide::Dire, &dire)] {
            for &discord_id in ids {
                let won = team == MatchSide::Radiant;
                let balance = if won { 100 + WIN_REWARD } else { 100 };
                connection
                    .execute(
                        "INSERT INTO players (
                             discord_id,discord_username,initial_mmr,wins,losses,
                             glicko_rating,glicko_rd,glicko_volatility,
                             jopacoin_balance,lowest_balance_ever,guild_id,
                             os_mu,os_sigma,os_rating_version
                         ) VALUES (?1,?2,4000,?3,?4,1500.0,350.0,0.06,
                                   ?5,?5,?6,35.0,8.333333333333334,5)",
                        params![
                            discord_id,
                            format!("player-{discord_id}"),
                            i64::from(won),
                            i64::from(!won),
                            balance,
                            GUILD
                        ],
                    )
                    .expect("seed player");
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
                    .expect("seed participant");
                connection
                    .execute(
                        "INSERT INTO rating_history (
                             discord_id,rating,rating_before,rd_before,rd_after,
                             volatility_before,volatility_after,team_number,won,
                             match_id,timestamp,os_mu_before,os_mu_after,
                             os_sigma_before,os_sigma_after,fantasy_weight,
                             streak_length,streak_multiplier,
                             streak_multiplier_per_game,streak_threshold,
                             base_rating_delta_multiplier,
                             low_priority_gain_multiplier,guild_id,
                             os_algorithm_version,os_algorithm_fingerprint
                         ) VALUES (
                             ?1,1500.0,1500.0,350.0,340.0,0.06,0.06,?2,?3,
                             ?4,datetime(?5,'unixepoch'),35.0,?6,
                             8.333333333333334,8.0,NULL,1,1.0,0.10,3,0.75,
                             1.0,?7,5,'seed-v5'
                         )",
                        params![
                            discord_id,
                            team.number(),
                            won,
                            match_id,
                            MATCH_AT,
                            if won { 36.0 } else { 34.0 },
                            GUILD
                        ],
                    )
                    .expect("seed rating history");
            }
        }
        Self {
            database,
            match_id,
            radiant,
            dire,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.database.path()).expect("open fixture database")
    }

    fn balance(&self, discord_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, GUILD],
                |row| row.get(0),
            )
            .expect("load player balance")
    }
}

/// Exact runtime counterpart of Python's replay-failure recovery contract.
/// The first call commits the visible correction and durable replay job, then
/// fails before replay.  The second call resumes `core_applied`, clears the
/// job, and proves every winner movement occurred once.
#[tokio::test]
async fn test_correction_replay_failure_has_idempotent_recovery() {
    let fixture = Fixture::new();
    let repository = MatchCorrectionRepository::new(fixture.database.path());
    let rewards = Arc::new(RepositoryReward {
        repository: repository.clone(),
        calls: AtomicUsize::new(0),
    });
    let control = ProductionAdminMatchCorrectionControl {
        repository,
        database_path: fixture.database.path().to_path_buf(),
        openskill: CamaOpenSkillSystem::default(),
        rating: CamaRatingSystem::default(),
        new_player_mmr_discount: 0,
        house_payout_multiplier: 1.0,
        vanity_tax_rate: 0.1,
        rewards: rewards.clone(),
        vanity_tax: Arc::new(NoVanityTax),
        replay_gate: Arc::new(FailFirstReplay(AtomicBool::new(true))),
    };
    let request = AdminCorrectMatchRequest {
        guild_id: GUILD,
        actor_id: ADMIN,
        match_id: fixture.match_id,
        correct_result: AdminCorrectMatchSide::Dire,
    };

    let first = control
        .correct_match(request)
        .await
        .expect_err("injected replay failure must escape");
    assert!(matches!(
        first,
        AdminCorrectMatchError::Unexpected(ref message)
            if message.contains("injected replay failure")
    ));
    let connection = fixture.connection();
    let persisted: (i64, String, String, i64) = connection
        .query_row(
            "SELECT m.winning_team,c.state,j.reason,
                    (SELECT COUNT(*) FROM match_corrections mc
                     WHERE mc.match_id=m.match_id)
             FROM matches m
             JOIN match_correction_claims c ON c.match_id=m.match_id
             JOIN openskill_replay_jobs j ON j.guild_id=m.guild_id
             WHERE m.match_id=?1",
            [fixture.match_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("load persisted failed-replay state");
    assert_eq!(
        persisted,
        (
            2,
            "core_applied".to_owned(),
            format!("match_correction:{}", fixture.match_id),
            1
        )
    );
    drop(connection);
    assert_eq!(rewards.calls.load(Ordering::SeqCst), 5);
    for &discord_id in &fixture.radiant {
        assert_eq!(fixture.balance(discord_id), 100);
    }
    for &discord_id in &fixture.dire {
        assert_eq!(fixture.balance(discord_id), 100 + WIN_REWARD);
    }

    let recovered = control
        .correct_match(request)
        .await
        .expect("retry recovers replay");
    assert!(recovered.replay_recovered);
    assert_eq!(recovered.correction_id, Some(1));
    assert_eq!(rewards.calls.load(Ordering::SeqCst), 5);
    let connection = fixture.connection();
    let durable_counts: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM match_corrections WHERE match_id=?1),
                 (SELECT COUNT(*) FROM match_correction_claims WHERE match_id=?1),
                 (SELECT COUNT(*) FROM openskill_replay_jobs WHERE guild_id=?2),
                 (SELECT COUNT(*) FROM economy_ledger_entries
                  WHERE guild_id=?2 AND source='match_win_bonus'
                    AND related_type='match_win_bonus' AND related_id=CAST(?1 AS TEXT))",
            params![fixture.match_id, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("load recovered durable state");
    assert_eq!(durable_counts, (1, 0, 0, 5));
    for &discord_id in &fixture.radiant {
        assert_eq!(fixture.balance(discord_id), 100);
    }
    for &discord_id in &fixture.dire {
        assert_eq!(fixture.balance(discord_id), 100 + WIN_REWARD);
    }
}

/// A hard exit can occur after the production reward adapter commits its
/// base/Communion/Blood-Pact movements but before the coordinator snapshots
/// their combined reversal amount. The ledger marker must prevent a second
/// payment without preventing reconstruction of that total snapshot.
#[tokio::test]
async fn pending_correction_recovers_total_reward_snapshot_after_adapter_only_crash() {
    let fixture = Fixture::new();
    let repository = MatchCorrectionRepository::new(fixture.database.path());
    let composite_player = fixture.dire[0];
    let rewards = Arc::new(CompositeRepositoryReward {
        repository: repository.clone(),
        composite_player,
        // Models a 100 gross reward after bankruptcy/vanity, Communion, and
        // a protection-aware Blood Pact produced an 86-JC balance delta.
        composite_delta: 86,
        calls: AtomicUsize::new(0),
    });
    // Model the exact crash seam: money + ledger committed, participant
    // snapshot still NULL, and a freshly owned correction claim is still
    // pending. Ordinary stale takeover cannot touch this lease; first READY
    // must release the now-dead process owner and resume it as an orphan.
    repository
        .claim_match_correction(
            fixture.match_id,
            Some(GUILD),
            MatchSide::Dire,
            "dead-before-participant-snapshot",
            unix_seconds(),
            CLAIM_STALE_AFTER_SECONDS,
        )
        .expect("seed pending correction claim");
    rewards
        .award_match_win_bonus(CorrectionWinRewardRequest {
            guild_id: GUILD,
            match_id: fixture.match_id,
            discord_id: composite_player,
            gross_reward: WIN_REWARD,
        })
        .expect("commit composite reward before simulated crash");
    assert_eq!(fixture.balance(composite_player), 100 + 86);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT win_bonus_jc FROM match_participants
                 WHERE match_id=?1 AND discord_id=?2 AND guild_id=?3",
                params![fixture.match_id, composite_player, GUILD],
                |row| row.get::<_, Option<i64>>(0),
            )
            .expect("missing pre-crash snapshot"),
        None
    );

    let control = Arc::new(ProductionAdminMatchCorrectionControl {
        repository,
        database_path: fixture.database.path().to_path_buf(),
        openskill: CamaOpenSkillSystem::default(),
        rating: CamaRatingSystem::default(),
        new_player_mmr_discount: 0,
        house_payout_multiplier: 1.0,
        vanity_tax_rate: 0.1,
        rewards: rewards.clone(),
        vanity_tax: Arc::new(NoVanityTax),
        replay_gate: Arc::new(AllowReplay),
    });
    let observer = AdminMatchCorrectionRecoveryObserver {
        control,
        startup_guild_leases_released: Mutex::new(BTreeSet::new()),
    };
    let ready = ReadyRecoveryContext::new([GUILD as u64], Arc::new(EmptyMemberSource));
    let report = observer.ready_recovery(ready).await;
    assert_eq!(report.guilds_attempted, 1);
    assert_eq!(report.guilds_refreshed, 1);
    assert!(report.failures.is_empty());

    // One pre-crash adapter call plus all five winners during recovery. The
    // composite player's exact-once ledger marker keeps its live balance at
    // one 86-JC credit while its missing total snapshot is repaired.
    assert_eq!(rewards.calls.load(Ordering::SeqCst), 6);
    assert_eq!(fixture.balance(composite_player), 100 + 86);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT win_bonus_jc FROM match_participants
                 WHERE match_id=?1 AND discord_id=?2 AND guild_id=?3",
                params![fixture.match_id, composite_player, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("recovered composite snapshot"),
        86
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM match_correction_claims WHERE match_id=?1",
                [fixture.match_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("completed orphan claim count"),
        0
    );
}

#[tokio::test]
async fn ready_observer_recovers_a_persisted_core_applied_correction() {
    let fixture = Fixture::new();
    let repository = MatchCorrectionRepository::new(fixture.database.path());
    let rewards = Arc::new(RepositoryReward {
        repository: repository.clone(),
        calls: AtomicUsize::new(0),
    });
    let control = Arc::new(ProductionAdminMatchCorrectionControl {
        repository,
        database_path: fixture.database.path().to_path_buf(),
        openskill: CamaOpenSkillSystem::default(),
        rating: CamaRatingSystem::default(),
        new_player_mmr_discount: 0,
        house_payout_multiplier: 1.0,
        vanity_tax_rate: 0.1,
        rewards: rewards.clone(),
        vanity_tax: Arc::new(NoVanityTax),
        replay_gate: Arc::new(FailFirstReplay(AtomicBool::new(true))),
    });
    let request = AdminCorrectMatchRequest {
        guild_id: GUILD,
        actor_id: ADMIN,
        match_id: fixture.match_id,
        correct_result: AdminCorrectMatchSide::Dire,
    };
    control
        .correct_match(request)
        .await
        .expect_err("seed persisted replay failure");

    // Model a hard process exit before Python/Rust's finally block could
    // release the claim. The timestamp is deliberately fresh: ordinary stale
    // takeover must refuse it, while single-process READY recovery must know
    // that its old owner is dead.
    fixture
        .connection()
        .execute(
            "UPDATE match_correction_claims
             SET owner_token='dead-process-owner',claimed_at=?1
             WHERE match_id=?2",
            params![unix_seconds(), fixture.match_id],
        )
        .expect("seed fresh dead-process lease");

    let observer = AdminMatchCorrectionRecoveryObserver {
        control: Arc::clone(&control),
        startup_guild_leases_released: Mutex::new(BTreeSet::new()),
    };
    let context = ReadyRecoveryContext::new([GUILD as u64], Arc::new(EmptyMemberSource));
    let report = observer.ready_recovery(context).await;
    assert_eq!(report.observer, RECOVERY_OBSERVER_NAME);
    assert_eq!(report.guilds_attempted, 1);
    assert_eq!(report.guilds_refreshed, 1);
    assert!(report.failures.is_empty());
    assert_eq!(rewards.calls.load(Ordering::SeqCst), 5);
    let remaining: (i64, i64) = fixture
        .connection()
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM match_correction_claims WHERE match_id=?1),
                 (SELECT COUNT(*) FROM openskill_replay_jobs WHERE guild_id=?2)",
            params![fixture.match_id, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("load READY recovery state");
    assert_eq!(remaining, (0, 0));
}

#[tokio::test]
async fn ready_observer_cleans_claim_when_replay_committed_before_process_exit() {
    let fixture = Fixture::new();
    let repository = MatchCorrectionRepository::new(fixture.database.path());
    let rewards = Arc::new(RepositoryReward {
        repository: repository.clone(),
        calls: AtomicUsize::new(0),
    });
    let control = Arc::new(ProductionAdminMatchCorrectionControl {
        repository: repository.clone(),
        database_path: fixture.database.path().to_path_buf(),
        openskill: CamaOpenSkillSystem::default(),
        rating: CamaRatingSystem::default(),
        new_player_mmr_discount: 0,
        house_payout_multiplier: 1.0,
        vanity_tax_rate: 0.1,
        rewards: rewards.clone(),
        vanity_tax: Arc::new(NoVanityTax),
        replay_gate: Arc::new(FailFirstReplay(AtomicBool::new(true))),
    });
    let request = AdminCorrectMatchRequest {
        guild_id: GUILD,
        actor_id: ADMIN,
        match_id: fixture.match_id,
        correct_result: AdminCorrectMatchSide::Dire,
    };
    control
        .correct_match(request)
        .await
        .expect_err("seed core-applied claim and replay job");
    repository
        .replay_openskill_atomic_configured(
            Some(GUILD),
            &CamaOpenSkillSystem::default(),
            &CamaRatingSystem::default(),
            0,
        )
        .expect("model replay committing before process exit");
    fixture
        .connection()
        .execute(
            "UPDATE match_correction_claims
             SET owner_token='dead-after-replay',claimed_at=?1
             WHERE match_id=?2",
            params![unix_seconds(), fixture.match_id],
        )
        .expect("seed fresh post-replay dead-process lease");

    let observer = AdminMatchCorrectionRecoveryObserver {
        control,
        startup_guild_leases_released: Mutex::new(BTreeSet::new()),
    };
    let context = ReadyRecoveryContext::new([GUILD as u64], Arc::new(EmptyMemberSource));
    let report = observer.ready_recovery(context).await;
    assert_eq!(report.guilds_attempted, 1);
    assert_eq!(report.guilds_refreshed, 1);
    assert!(report.failures.is_empty());
    assert_eq!(rewards.calls.load(Ordering::SeqCst), 5);
    let remaining: (i64, i64, i64) = fixture
        .connection()
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM match_correction_claims WHERE match_id=?1),
                 (SELECT COUNT(*) FROM openskill_replay_jobs WHERE guild_id=?2),
                 (SELECT COUNT(*) FROM match_corrections WHERE match_id=?1)",
            params![fixture.match_id, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("load post-replay startup recovery state");
    assert_eq!(remaining, (0, 0, 1));
}

#[tokio::test]
async fn reconnect_ready_never_releases_a_live_correction_lease() {
    let fixture = Fixture::new();
    let repository = MatchCorrectionRepository::new(fixture.database.path());
    let rewards = Arc::new(RepositoryReward {
        repository: repository.clone(),
        calls: AtomicUsize::new(0),
    });
    let control = Arc::new(ProductionAdminMatchCorrectionControl {
        repository: repository.clone(),
        database_path: fixture.database.path().to_path_buf(),
        openskill: CamaOpenSkillSystem::default(),
        rating: CamaRatingSystem::default(),
        new_player_mmr_discount: 0,
        house_payout_multiplier: 1.0,
        vanity_tax_rate: 0.1,
        rewards,
        vanity_tax: Arc::new(NoVanityTax),
        replay_gate: Arc::new(AllowReplay),
    });
    let observer = AdminMatchCorrectionRecoveryObserver {
        control,
        startup_guild_leases_released: Mutex::new(BTreeSet::new()),
    };
    let ready = || ReadyRecoveryContext::new([GUILD as u64], Arc::new(EmptyMemberSource));
    let first = observer.ready_recovery(ready()).await;
    assert!(first.failures.is_empty());

    repository
        .claim_match_correction(
            fixture.match_id,
            Some(GUILD),
            MatchSide::Dire,
            "live-command-owner",
            unix_seconds(),
            CLAIM_STALE_AFTER_SECONDS,
        )
        .expect("seed live command lease after initial READY");
    let resumed = observer.ready_recovery(ready()).await;
    assert!(resumed.failures.is_empty());
    let owner: Option<String> = fixture
        .connection()
        .query_row(
            "SELECT owner_token FROM match_correction_claims WHERE match_id=?1",
            [fixture.match_id],
            |row| row.get(0),
        )
        .expect("load live owner after reconnect READY");
    assert_eq!(owner.as_deref(), Some("live-command-owner"));
}

#[tokio::test]
async fn ready_observer_leaves_foreign_guild_correction_and_lease_untouched() {
    let fixture = Fixture::new();
    let repository = MatchCorrectionRepository::new(fixture.database.path());
    let rewards = Arc::new(RepositoryReward {
        repository: repository.clone(),
        calls: AtomicUsize::new(0),
    });
    let control = Arc::new(ProductionAdminMatchCorrectionControl {
        repository: repository.clone(),
        database_path: fixture.database.path().to_path_buf(),
        openskill: CamaOpenSkillSystem::default(),
        rating: CamaRatingSystem::default(),
        new_player_mmr_discount: 0,
        house_payout_multiplier: 1.0,
        vanity_tax_rate: 0.1,
        rewards: rewards.clone(),
        vanity_tax: Arc::new(NoVanityTax),
        replay_gate: Arc::new(FailFirstReplay(AtomicBool::new(true))),
    });
    control
        .correct_match(AdminCorrectMatchRequest {
            guild_id: GUILD,
            actor_id: ADMIN,
            match_id: fixture.match_id,
            correct_result: AdminCorrectMatchSide::Dire,
        })
        .await
        .expect_err("seed recoverable foreign correction");
    fixture
        .connection()
        .execute(
            "UPDATE match_correction_claims
             SET owner_token='foreign-dead-owner',claimed_at=?1
             WHERE match_id=?2",
            params![unix_seconds(), fixture.match_id],
        )
        .expect("seed foreign dead-process lease");
    let calls_before_ready = rewards.calls.load(Ordering::SeqCst);
    let observer = AdminMatchCorrectionRecoveryObserver {
        control,
        startup_guild_leases_released: Mutex::new(BTreeSet::new()),
    };

    let report = observer
        .ready_recovery(ReadyRecoveryContext::new(
            [(GUILD + 1) as u64],
            Arc::new(EmptyMemberSource),
        ))
        .await;

    assert!(report.failures.is_empty());
    assert_eq!(report.guilds_attempted, 1);
    assert_eq!(report.guilds_refreshed, 0);
    assert_eq!(rewards.calls.load(Ordering::SeqCst), calls_before_ready);
    let state: (String, i64, i64) = fixture
        .connection()
        .query_row(
            "SELECT owner_token,
                    (SELECT COUNT(*) FROM match_correction_claims WHERE match_id=?1),
                    (SELECT COUNT(*) FROM openskill_replay_jobs WHERE guild_id=?2)
             FROM match_correction_claims WHERE match_id=?1",
            params![fixture.match_id, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("foreign recovery state remains");
    assert_eq!(state, ("foreign-dead-owner".to_owned(), 1, 1));
}
