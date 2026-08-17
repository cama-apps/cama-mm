use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cama_app::ai_services::{
    AIService, AiProvider, FLAVOR_TOOL_NAME, ProviderConfig, ProviderCredential, ProviderError,
    ProviderLoader, ProviderRequest, ProviderResponse, ToolCall, Value,
};
use cama_app::betting_reminder_messaging::{BettingFlavorPort, FlavorKind, FlavorRequest};
use cama_db::betting_service_repository::{BettingServiceRepositoryError, PlaceBetRequest};
use cama_db::guild_config_repository::GuildConfigRepository;
use cama_db::schema_manager::initialize_or_migrate;
use cama_domain::guild_config::GuildConfigStore;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use crate::discord_transport::DiscordAllowedMentions;
use crate::registration::{InteractionAllowedMentions, InteractionResponseError};

use super::*;

const GUILD: i64 = 82_001;
const MATCH_ID: i64 = 91_001;
const WINNER: i64 = 71_001;
const SKIMMER: i64 = 71_002;

struct NoVanityTax;

impl MatchVanityTaxSource for NoVanityTax {
    fn taxable_ids(&self, _guild_id: i64) -> BTreeSet<i64> {
        BTreeSet::new()
    }
}

fn reward_fixture() -> (NamedTempFile, ProductionMatchRewardControl) {
    let database = NamedTempFile::new().expect("temporary match reward database");
    initialize_or_migrate(database.path()).expect("migrate canonical schema");
    let connection = Connection::open(database.path()).expect("open reward database");
    connection
        .execute_batch("PRAGMA foreign_keys=OFF")
        .expect("use migrated legacy foreign-key policy");
    for (discord_id, balance) in [(WINNER, -100), (SKIMMER, 0)] {
        connection
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,jopacoin_balance
                 ) VALUES(?1,?2,?3,?4)",
                params![discord_id, GUILD, format!("reward-{discord_id}"), balance],
            )
            .expect("insert reward player");
    }
    connection
        .execute(
            "INSERT INTO matches(
                 match_id,team1_players,team2_players,winning_team,guild_id
             ) VALUES(?1,'[]','[]',1,?2)",
            params![MATCH_ID, GUILD],
        )
        .expect("insert attributed match");
    connection
        .execute(
            "INSERT INTO bankruptcy_state(
                 discord_id,guild_id,last_bankruptcy_at,
                 penalty_games_remaining,bankruptcy_count
             ) VALUES(?1,?2,0,2,1)",
            params![WINNER, GUILD],
        )
        .expect("insert bankruptcy penalty");
    connection
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data
             ) VALUES(?1,?2,'communion_blessing',0,?3,0,'{}')",
            params![WINNER, GUILD, i64::MAX],
        )
        .expect("insert Communion blessing");
    connection
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,target_id,
                 granted_at,expires_at,triggered,data
             ) VALUES(?1,?2,'blood_pact',?3,0,?4,0,?5)",
            params![
                SKIMMER,
                GUILD,
                WINNER,
                i64::MAX,
                r#"{"skim_rate":0.25,"cap":100,"skimmed_total":0}"#
            ],
        )
        .expect("insert Blood Pact");
    drop(connection);

    let economy = EconomyEventConfig {
        enabled: false,
        ..EconomyEventConfig::default()
    };
    let control = ProductionMatchRewardControl {
        database_path: database.path().to_path_buf(),
        garnishment_rate: 0.5,
        bankruptcy_kept_rate: 0.75,
        vanity_tax_rate: 0.10,
        minigame_scale: 1.0,
        vanity_tax: Arc::new(NoVanityTax),
        economy: Arc::new(SqliteEconomyEventService::new(database.path(), economy)),
    };
    (database, control)
}

fn blood_pact_cap_fixture(
    minigame_scale: f64,
    skimmed_total: i64,
) -> (NamedTempFile, ProductionMatchRewardControl) {
    let database = NamedTempFile::new().expect("temporary Blood Pact cap database");
    initialize_or_migrate(database.path()).expect("migrate canonical schema");
    let connection = Connection::open(database.path()).expect("open Blood Pact cap database");
    connection
        .execute_batch("PRAGMA foreign_keys=OFF")
        .expect("use migrated legacy foreign-key policy");
    for (discord_id, balance) in [(WINNER, 500), (SKIMMER, 0)] {
        connection
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,jopacoin_balance
                 ) VALUES(?1,?2,?3,?4)",
                params![discord_id, GUILD, format!("pact-{discord_id}"), balance],
            )
            .expect("insert Blood Pact cap player");
    }
    connection
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,target_id,
                 granted_at,expires_at,triggered,data
             ) VALUES(?1,?2,'blood_pact',?3,0,?4,0,?5)",
            params![
                SKIMMER,
                GUILD,
                WINNER,
                i64::MAX,
                format!(r#"{{"skim_rate":0.25,"cap":150,"skimmed_total":{skimmed_total}}}"#)
            ],
        )
        .expect("insert Blood Pact cap state");
    drop(connection);

    let economy = EconomyEventConfig {
        enabled: false,
        ..EconomyEventConfig::default()
    };
    let control = ProductionMatchRewardControl {
        database_path: database.path().to_path_buf(),
        garnishment_rate: 0.5,
        bankruptcy_kept_rate: 0.75,
        vanity_tax_rate: 0.10,
        minigame_scale,
        vanity_tax: Arc::new(NoVanityTax),
        economy: Arc::new(SqliteEconomyEventService::new(database.path(), economy)),
    };
    (database, control)
}

fn blood_pact_cap_state(database: &NamedTempFile) -> (i64, i64, i64) {
    Connection::open(database.path())
        .expect("inspect Blood Pact cap state")
        .query_row(
            "SELECT
                 (SELECT jopacoin_balance FROM players
                  WHERE guild_id=?1 AND discord_id=?2),
                 (SELECT jopacoin_balance FROM players
                  WHERE guild_id=?1 AND discord_id=?3),
                 (SELECT CAST(json_extract(data,'$.skimmed_total') AS INTEGER)
                  FROM manashop_buffs
                  WHERE guild_id=?1 AND discord_id=?3 AND target_id=?2
                    AND buff_type='blood_pact')",
            params![GUILD, WINNER, SKIMMER],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read Blood Pact cap state")
}

#[test]
fn test_scaled_skim_counts_extra_against_cap() {
    let (database, control) = blood_pact_cap_fixture(2.0, 0);
    assert_eq!(
        control
            .apply_attributed_blood_pact(
                WINNER,
                GUILD,
                100,
                "pact-cap:scaled-extra".to_owned(),
                1_700_000_000,
            )
            .expect("apply scaled Blood Pact"),
        50
    );
    assert_eq!(blood_pact_cap_state(&database), (450, 50, 50));
}

#[test]
fn test_scaled_skim_clamps_transfer_at_cap() {
    let (database, control) = blood_pact_cap_fixture(2.0, 100);
    assert_eq!(
        control
            .apply_attributed_blood_pact(
                WINNER,
                GUILD,
                400,
                "pact-cap:clamped".to_owned(),
                1_700_000_000,
            )
            .expect("apply clamped Blood Pact"),
        50
    );
    assert_eq!(blood_pact_cap_state(&database), (450, 50, 150));
    assert_eq!(
        control
            .apply_attributed_blood_pact(
                WINNER,
                GUILD,
                400,
                "pact-cap:exhausted".to_owned(),
                1_700_000_001,
            )
            .expect("retry exhausted Blood Pact"),
        0
    );
    assert_eq!(blood_pact_cap_state(&database), (450, 50, 150));
}

#[test]
fn test_partial_headroom_grants_partial_extra() {
    let (database, control) = blood_pact_cap_fixture(2.0, 60);
    assert_eq!(
        control
            .apply_attributed_blood_pact(
                WINNER,
                GUILD,
                200,
                "pact-cap:partial-extra".to_owned(),
                1_700_000_000,
            )
            .expect("apply partial scaled Blood Pact"),
        90
    );
    assert_eq!(blood_pact_cap_state(&database), (410, 90, 150));
}

#[test]
fn test_unscaled_skim_accounting_unchanged() {
    let (database, control) = blood_pact_cap_fixture(1.0, 0);
    assert_eq!(
        control
            .apply_attributed_blood_pact(
                WINNER,
                GUILD,
                100,
                "pact-cap:unscaled".to_owned(),
                1_700_000_000,
            )
            .expect("apply unscaled Blood Pact"),
        25
    );
    assert_eq!(blood_pact_cap_state(&database), (475, 25, 25));
}

#[test]
fn migrated_correction_retry_recovers_communion_and_protected_pact_snapshot_exactly() {
    let (database, control) = reward_fixture();
    let request = CorrectionWinRewardRequest {
        guild_id: GUILD,
        match_id: MATCH_ID,
        discord_id: WINNER,
        gross_reward: 100,
    };

    let first = control
        .award_match_win_bonus(request)
        .expect("first correction reward");
    // Base: +100 - 12 bankruptcy = +88 balance delta (50 garnished);
    // Communion: +10; Blood Pact: -12 from spendable 38+10.
    assert_eq!(first.snapshot_balance_delta, 86);

    // Simulate a process crash before the admin coordinator writes
    // match_participants.win_bonus_jc: call the adapter again with only its
    // ledger/Communion/hostile-loss state available.
    let retry = control
        .award_match_win_bonus(request)
        .expect("recovered correction reward");
    assert_eq!(retry, first);

    let connection = Connection::open(database.path()).expect("inspect reward state");
    let balances = connection
        .prepare(
            "SELECT discord_id,jopacoin_balance FROM players
             WHERE guild_id=?1 ORDER BY discord_id",
        )
        .expect("prepare balances")
        .query_map([GUILD], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("read balances")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect balances");
    assert_eq!(balances, vec![(WINNER, -14), (SKIMMER, 12)]);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM hostile_loss_events
                 WHERE event_key=?1",
                [format!("match-win-blood-pact:{MATCH_ID}:{WINNER}")],
                |row| row.get::<_, i64>(0),
            )
            .expect("count pact events"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COALESCE(SUM(delta),0) FROM economy_ledger_entries
                 WHERE guild_id=?1 AND account_id=?2
                   AND related_type='match_win_bonus' AND related_id=?3",
                params![GUILD, WINNER, MATCH_ID.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("recover attributed credits"),
        98
    );
}

fn production_test_config() -> ApplicationConfig {
    let mut config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ENRICHMENT_RETRY_DELAYS" => Some("0".to_owned()),
        _ => None,
    })
    .expect("production match test config");
    config.values.bet_reminder_offsets = vec![60];
    config.values.bet_last_call_offset = 30;
    config.values.jopacoin_win_reward = 20;
    config.values.jopacoin_per_game = 2;
    config.values.jopacoin_exclusion_reward = 1;
    config
}

struct MatchRuntimeFixture {
    database: NamedTempFile,
    lobby: crate::lobby_provider::LobbyRegistrationProvider,
    drafts: Arc<cama_app::draft::DraftStateManager>,
    provider: MatchRegistrationProvider,
}

impl MatchRuntimeFixture {
    fn new() -> Self {
        Self::new_with_discord(Arc::new(PublicationDiscord::default()))
    }

    fn new_with_discord(discord: Arc<dyn DiscordTransport>) -> Self {
        Self::new_with_config_and_discord(production_test_config(), discord)
    }

    fn new_with_config_and_discord(
        config: ApplicationConfig,
        discord: Arc<dyn DiscordTransport>,
    ) -> Self {
        use crate::lobby_provider::{LobbyRegistrationProvider, LobbyRuntimeConfig};
        use cama_app::draft::DraftStateManager;
        use cama_app::service_container::ServiceContainer;

        let database = NamedTempFile::new().expect("temporary production match database");
        initialize_or_migrate(database.path()).expect("migrate production match fixture");
        let drafts = Arc::new(DraftStateManager::default());
        let lobby = LobbyRegistrationProvider::new(
            database.path(),
            LobbyRuntimeConfig::from_application_config(&config).expect("production lobby config"),
            Arc::clone(&drafts),
            Arc::clone(&discord),
        )
        .expect("compose shared lobby fixture");
        let mut services =
            ServiceContainer::new(database.path(), config.service_container_options(None));
        services.initialize();
        let vanity = Arc::clone(
            &services
                .components()
                .expect("service container components")
                .vanity_tax_service,
        );
        let discovery: Arc<dyn RecordedMatchDiscovery> = Arc::new(StaticRecordedDiscovery {
            outcome: RecordedMatchDiscoveryOutcome::Disabled,
        });
        let provider = MatchRegistrationProvider::new(
            database.path(),
            &config,
            vanity,
            lobby.match_lobby_port(),
            discovery,
            discord,
        )
        .expect("compose production match provider");
        Self {
            database,
            lobby,
            drafts,
            provider,
        }
    }

    async fn dispatch_lobby_command(
        &self,
        name: &str,
        user_id: i64,
        lobby_kind: LobbyKind,
    ) -> Arc<RecordingMatchResponder> {
        let mut builder = RegistryBuilder::default();
        self.lobby
            .register(&mut builder)
            .expect("register fixture lobby provider");
        let responder = Arc::new(RecordingMatchResponder::default());
        builder
            .build()
            .command_handler(name)
            .expect("fixture lobby command")
            .handle(
                InteractionRequest::Command {
                    interaction_id: u64::try_from(user_id).expect("fixture Discord user"),
                    name: name.to_owned(),
                    user_id: u64::try_from(user_id).expect("fixture Discord user"),
                    user_display_name: format!("Player {user_id}"),
                    guild_id: Some(u64::try_from(GUILD).expect("fixture Discord guild")),
                    channel_id: Some(77_001),
                    member_permissions: None,
                    options: vec![InteractionOption {
                        name: "lobby".to_owned(),
                        value: InteractionValue::String(lobby_kind_value(lobby_kind).to_owned()),
                    }],
                },
                responder.clone(),
            )
            .await
            .expect("dispatch fixture lobby command");
        responder
    }

    async fn populate_lobby(&self, player_ids: &[i64], lobby_kind: LobbyKind) {
        let Some((&creator, remaining)) = player_ids.split_first() else {
            panic!("fixture lobby cannot be empty");
        };
        let created = self
            .dispatch_lobby_command("lobby", creator, lobby_kind)
            .await;
        assert!(
            !created
                .contents()
                .iter()
                .any(|content| content.starts_with('❌')),
            "fixture lobby creation failed: {:?}",
            created.contents()
        );
        for player_id in remaining {
            let joined = self
                .dispatch_lobby_command("join", *player_id, lobby_kind)
                .await;
            assert!(
                !joined
                    .contents()
                    .iter()
                    .any(|content| content.starts_with('❌')),
                "fixture lobby join failed: {:?}",
                joined.contents()
            );
        }
    }

    fn pending(&self, lock_until: i64) -> PendingMatchRecord {
        let players = PlayerRepository::new(self.database.path());
        let radiant = (81_100_i64..81_105).collect::<Vec<_>>();
        let dire = (81_105_i64..81_110).collect::<Vec<_>>();
        for discord_id in radiant.iter().chain(&dire) {
            let mut player =
                NewPlayer::new(*discord_id, format!("ready-{discord_id}"), Some(GUILD));
            player.initial_mmr = Some(4_000);
            player.glicko_rating = Some(1_500.0);
            player.glicko_rd = Some(200.0);
            player.glicko_volatility = Some(0.06);
            player.os_mu = Some(CamaOpenSkillSystem::DEFAULT_MU);
            player.os_sigma = Some(CamaOpenSkillSystem::DEFAULT_SIGMA);
            players.add(&player).expect("insert READY player");
        }
        PendingMatchRepository::new(self.database.path())
            .create_pending_match(
                GUILD,
                &PendingMatchState {
                    radiant_team_ids: radiant,
                    dire_team_ids: dire,
                    shuffle_timestamp: Some(unix_seconds()),
                    lock_time: Some(lock_until),
                    bet_lock_until: Some(lock_until),
                    lobby_kind: Some("open".to_owned()),
                    origin_channel_id: Some(77_001),
                    ..PendingMatchState::default()
                },
            )
            .expect("insert READY pending match")
    }

    fn final_low_priority_pending(&self) -> (PendingMatchRecord, i64, i64) {
        let pending = self.pending(unix_seconds() + 120);
        let target = pending.state.radiant_team_ids[0];
        let peer = pending.state.radiant_team_ids[1];
        Connection::open(self.database.path())
            .expect("open low-priority fixture")
            .execute(
                "INSERT INTO low_priority_state (
                    discord_id,guild_id,wins_required,wins_remaining,
                    start_pending_match_id,active,set_by
                 ) VALUES (?1,?2,1,1,NULL,1,901)",
                params![target, GUILD],
            )
            .expect("seed final low-priority win");
        (pending, target, peer)
    }

    fn add_shuffle_pool(&self, count: usize, with_openskill: bool) -> Vec<i64> {
        self.add_shuffle_pool_from(84_000, count, with_openskill)
    }

    fn add_shuffle_pool_from(
        &self,
        first_discord_id: i64,
        count: usize,
        with_openskill: bool,
    ) -> Vec<i64> {
        let repository = PlayerRepository::new(self.database.path());
        (0..count)
            .map(|index| {
                let discord_id = first_discord_id + i64::try_from(index).expect("small fixture");
                let mut player =
                    NewPlayer::new(discord_id, format!("shuffle-{discord_id}"), Some(GUILD));
                player.initial_mmr = Some(3_000);
                player.preferred_roles = Some(vec![
                    "1".to_owned(),
                    "2".to_owned(),
                    "3".to_owned(),
                    "4".to_owned(),
                    "5".to_owned(),
                ]);
                player.glicko_rating = Some(1_500.0);
                player.glicko_rd = Some(350.0);
                player.glicko_volatility = Some(0.06);
                if with_openskill {
                    player.os_mu = Some(30.0);
                    player.os_sigma = Some(8.0);
                }
                repository.add(&player).expect("insert shuffle player");
                discord_id
            })
            .collect()
    }

    fn set_glicko_rating(&self, player_ids: &[i64], rating: f64) {
        let mut connection = Connection::open(self.database.path()).expect("open rating fixture");
        let transaction = connection.transaction().expect("start rating fixture");
        for player_id in player_ids {
            transaction
                .execute(
                    "UPDATE players SET glicko_rating=?1 WHERE guild_id=?2 AND discord_id=?3",
                    params![rating, GUILD, player_id],
                )
                .expect("update fixture rating");
        }
        transaction.commit().expect("commit fixture ratings");
    }

    fn prepare_shuffle(
        &self,
        player_ids: Vec<i64>,
        rating_system: &str,
        excluded_conditional_ids: Vec<i64>,
    ) -> PreparedShuffle {
        self.provider
            .handler
            .prepare_shuffle(PrepareShuffleRequest {
                guild_id: GUILD,
                lobby_kind: LobbyKind::Open,
                player_ids,
                excluded_conditional_ids,
                lobby_wait_minutes: HashMap::new(),
                rating_system: rating_system.to_owned(),
                shuffle_mode: "balanced".to_owned(),
                shuffle_timestamp: unix_seconds(),
                is_bomb_pot: false,
            })
            .expect("prepare production shuffle")
    }
}

#[derive(Default)]
struct RecordingMatchResponder {
    acknowledged: AtomicBool,
    responses: Mutex<Vec<InteractionResponse>>,
}

impl RecordingMatchResponder {
    fn contents(&self) -> Vec<String> {
        self.responses
            .lock()
            .expect("match responder lock")
            .iter()
            .map(|response| response.content.clone())
            .collect()
    }
}

#[async_trait]
impl InteractionResponder for RecordingMatchResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.acknowledged.store(true, Ordering::Release);
        self.responses
            .lock()
            .expect("match responder lock")
            .push(response);
        Ok(())
    }

    async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.acknowledged.store(true, Ordering::Release);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.responses
            .lock()
            .expect("match responder lock")
            .push(response);
        Ok(())
    }

    fn response_is_done(&self) -> bool {
        self.acknowledged.load(Ordering::Acquire)
    }
}

fn record_context(
    user_id: i64,
    guild_id: i64,
    result: &str,
    is_admin: bool,
) -> MatchCommandContext {
    MatchCommandContext {
        user_id,
        guild_id,
        channel_id: Some(77_001),
        member_permissions: is_admin.then_some(ADMINISTRATOR_PERMISSION),
        options: vec![InteractionOption {
            name: "result".to_owned(),
            value: InteractionValue::String(result.to_owned()),
        }],
    }
}

#[test]
fn test_load_glicko_player_applies_decay_after_grace_period() {
    let system = CamaRatingSystem::default();
    let now = DateTime::parse_from_rfc3339("2026-08-12T12:00:00Z")
        .expect("fixed timestamp")
        .with_timezone(&Utc);
    let decayed = decayed_glicko_rd(
        &system,
        100.0,
        Some(now - ChronoDuration::days(21)),
        None,
        now,
    );
    let expected = (100.0_f64.powi(2) + 100.0_f64.powi(2) * 2.0).sqrt();
    assert!((decayed - expected).abs() < 1.0e-6);
}

#[test]
fn test_load_glicko_player_uses_created_at_when_last_match_missing() {
    let system = CamaRatingSystem::default();
    let now = DateTime::parse_from_rfc3339("2026-08-12T12:00:00Z")
        .expect("fixed timestamp")
        .with_timezone(&Utc);
    assert!(
        decayed_glicko_rd(
            &system,
            120.0,
            None,
            Some(now - ChronoDuration::days(21)),
            now,
        ) > 120.0
    );
}

#[test]
fn test_load_glicko_player_caps_rd_at_350() {
    let system = CamaRatingSystem::default();
    let now = DateTime::parse_from_rfc3339("2026-08-12T12:00:00Z")
        .expect("fixed timestamp")
        .with_timezone(&Utc);
    assert_eq!(
        decayed_glicko_rd(
            &system,
            340.0,
            Some(now - ChronoDuration::days(70)),
            None,
            now,
        ),
        350.0
    );
}

fn assert_shuffle_prediction_preserves_zero_and_uses_discounted_missing_seed(
    special_rating: Option<f64>,
    special_mmr: i64,
) {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool_from(92_001, 10, false);
    Connection::open(fixture.database.path())
        .expect("open shuffle prediction fixture")
        .execute(
            "UPDATE players
             SET initial_mmr=?1,current_mmr=?1,glicko_rating=?2
             WHERE guild_id=?3 AND discord_id=?4",
            params![special_mmr, special_rating, GUILD, player_ids[0]],
        )
        .expect("seed special shuffle prediction player");

    let prepared = fixture.prepare_shuffle(player_ids.clone(), "glicko", Vec::new());
    let players = fixture
        .provider
        .handler
        .players
        .get_by_ids(&player_ids, Some(GUILD))
        .expect("load shuffled prediction players");
    let system = CamaRatingSystem::default();
    let player_for = |discord_id: i64| {
        players
            .iter()
            .find(|player| player.discord_id == Some(discord_id))
            .unwrap_or_else(|| panic!("missing prediction player {discord_id}"))
    };
    let aggregate = |team_ids: &[i64]| {
        let ratings = team_ids
            .iter()
            .map(|discord_id| {
                let player = player_for(*discord_id);
                player.glicko_rating.map_or_else(
                    || {
                        system.create_player_from_mmr(
                            player.mmr.and_then(|mmr| i32::try_from(mmr).ok()),
                        )
                    },
                    |rating| {
                        system.create_player_from_rating(
                            rating,
                            player.glicko_rd.unwrap_or(system.initial_rd),
                            player
                                .glicko_volatility
                                .unwrap_or(system.initial_volatility),
                        )
                    },
                )
            })
            .collect::<Vec<_>>();
        system.aggregate_team_stats(&ratings)
    };
    let radiant = aggregate(&prepared.pending.state.radiant_team_ids);
    let dire = aggregate(&prepared.pending.state.dire_team_ids);
    let expected =
        CamaRatingSystem::predict_win_probability(radiant.rating, radiant.rd, dire.rating, dire.rd);
    let actual = prepared
        .pending
        .state
        .extra
        .get("glicko_radiant_win_prob")
        .and_then(serde_json::Value::as_f64)
        .expect("persisted Glicko shuffle prediction");
    assert!((actual - expected).abs() < 1.0e-12);

    let special = player_for(player_ids[0]);
    assert_eq!(special.mmr, Some(special_mmr));
    assert_eq!(special.glicko_rating, special_rating);
}

#[test]
fn test_shuffle_prediction_preserves_zero_rating_seed() {
    assert_shuffle_prediction_preserves_zero_and_uses_discounted_missing_seed(Some(0.0), 500);
}

#[test]
fn test_shuffle_prediction_uses_discounted_missing_rating_seed() {
    assert_shuffle_prediction_preserves_zero_and_uses_discounted_missing_seed(None, 8_000);
}

fn assert_first_pick_choice_is_returned_and_persisted(radiant: bool) {
    let database = NamedTempFile::new().expect("first-pick database");
    initialize_or_migrate(database.path()).expect("migrate first-pick database");
    let repository = PendingMatchRepository::new(database.path());
    let expected = first_pick_team(radiant);
    let pending = repository
        .create_pending_match(
            GUILD,
            &PendingMatchState {
                first_pick_team: Some(expected.to_owned()),
                ..PendingMatchState::default()
            },
        )
        .expect("persist first-pick choice");
    assert_eq!(pending.state.first_pick_team.as_deref(), Some(expected));
    assert_eq!(
        repository
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read first-pick state")
            .expect("first-pick state exists")
            .state
            .first_pick_team
            .as_deref(),
        Some(expected)
    );
}

#[test]
fn test_firstpick_choice_is_returned_and_persisted_radiant() {
    assert_first_pick_choice_is_returned_and_persisted(true);
}

#[test]
fn test_firstpick_choice_is_returned_and_persisted_dire() {
    assert_first_pick_choice_is_returned_and_persisted(false);
}

#[test]
fn test_firstpick_assignment_multiple_guilds() {
    let database = NamedTempFile::new().expect("multi-guild first-pick database");
    initialize_or_migrate(database.path()).expect("migrate first-pick database");
    let repository = PendingMatchRepository::new(database.path());
    let radiant = repository
        .create_pending_match(
            100,
            &PendingMatchState {
                first_pick_team: Some(first_pick_team(true).to_owned()),
                ..PendingMatchState::default()
            },
        )
        .expect("persist guild 100 first pick");
    let dire = repository
        .create_pending_match(
            200,
            &PendingMatchState {
                first_pick_team: Some(first_pick_team(false).to_owned()),
                ..PendingMatchState::default()
            },
        )
        .expect("persist guild 200 first pick");
    assert_eq!(radiant.state.first_pick_team.as_deref(), Some("Radiant"));
    assert_eq!(dire.state.first_pick_team.as_deref(), Some("Dire"));
    assert_eq!(
        repository
            .pending_match(100, radiant.pending_match_id)
            .expect("read guild 100")
            .expect("guild 100 pending")
            .state
            .first_pick_team
            .as_deref(),
        Some("Radiant")
    );
    assert_eq!(
        repository
            .pending_match(200, dire.pending_match_id)
            .expect("read guild 200")
            .expect("guild 200 pending")
            .state
            .first_pick_team
            .as_deref(),
        Some("Dire")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_firstpick_persists_through_record_workflow() {
    let fixture = MatchRuntimeFixture::new();
    let mut pending = fixture.pending(unix_seconds() + 120);
    pending.state.first_pick_team = Some(first_pick_team(fastrand::bool()).to_owned());
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .update_pending_match(GUILD, pending.pending_match_id, &pending.state)
            .expect("persist workflow first pick")
    );
    let first_pick = pending
        .state
        .first_pick_team
        .clone()
        .expect("shuffle assigns first pick");
    assert!(matches!(first_pick.as_str(), "Radiant" | "Dire"));
    assert_eq!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read shuffled state")
            .expect("shuffled state exists")
            .state
            .first_pick_team,
        Some(first_pick)
    );
    let responder = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_record(record_context(99_001, GUILD, "radiant", true), responder)
        .await
        .expect("record first-pick match");
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read recorded state")
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_record_match_updates_wins_and_clears_state() {
    let fixture = MatchRuntimeFixture::new();
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .handle_record(
            record_context(99_001, GUILD, "dire", true),
            responder.clone(),
        )
        .await
        .expect("missing shuffle is a friendly no-op");
    assert_eq!(
        responder
            .responses
            .lock()
            .expect("missing-shuffle response")[0]
            .content,
        "❌ No pending match to record."
    );

    let pending = fixture.pending(unix_seconds() + 120);
    let radiant = pending.state.radiant_team_ids.clone();
    let dire = pending.state.dire_team_ids.clone();
    fixture
        .provider
        .handler
        .handle_record(
            record_context(99_001, GUILD, "dire", true),
            Arc::new(RecordingMatchResponder::default()),
        )
        .await
        .expect("record persisted Dire win");

    let players = PlayerRepository::new(fixture.database.path());
    for discord_id in &dire {
        let player = players
            .get_by_id(*discord_id, Some(GUILD))
            .expect("read Dire winner")
            .expect("Dire winner remains registered");
        assert_eq!((player.wins, player.losses), (1, 0));
    }
    for discord_id in &radiant {
        let player = players
            .get_by_id(*discord_id, Some(GUILD))
            .expect("read Radiant loser")
            .expect("Radiant loser remains registered");
        assert_eq!((player.wins, player.losses), (0, 1));
    }
    let pending_repository = PendingMatchRepository::new(fixture.database.path());
    assert!(
        pending_repository
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read cleared pending match")
            .is_none()
    );

    fixture
        .provider
        .handler
        .handle_record(
            record_context(99_001, GUILD, "dire", true),
            Arc::new(RecordingMatchResponder::default()),
        )
        .await
        .expect("duplicate record is a friendly no-op");
    let connection = Connection::open(fixture.database.path()).expect("inspect recorded match");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM matches WHERE guild_id=?1",
                [GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("count recorded matches"),
        1
    );
    for discord_id in &dire {
        let player = players
            .get_by_id(*discord_id, Some(GUILD))
            .expect("reread Dire winner")
            .expect("Dire winner remains registered");
        assert_eq!((player.wins, player.losses), (1, 0));
    }
    for discord_id in &radiant {
        let player = players
            .get_by_id(*discord_id, Some(GUILD))
            .expect("reread Radiant loser")
            .expect("Radiant loser remains registered");
        assert_eq!((player.wins, player.losses), (0, 1));
    }
}

#[test]
fn test_openskill_balanced_team_line_uses_openskill_display_and_certainty() {
    let openskill = CamaOpenSkillSystem::new();
    let player = Player {
        glicko_rating: Some(1_777.0),
        os_mu: Some(45.0),
        os_sigma: Some(4.0),
        ..Player::new("Player")
    };
    let label = balanced_player_rating_label(
        &player,
        "openskill",
        &CamaRatingSystem::default(),
        &openskill,
    );
    assert_eq!(
        label,
        format!(
            "{} | {:.0}%",
            openskill.mu_to_display(45.0),
            openskill.certainty_percentage(4.0)
        )
    );
    assert!(!label.contains("1777"));
}

#[test]
fn test_glicko_balanced_team_line_keeps_glicko_display() {
    let player = Player {
        glicko_rating: Some(1_777.0),
        os_mu: Some(45.0),
        os_sigma: Some(4.0),
        ..Player::new("Player")
    };
    assert_eq!(
        balanced_player_rating_label(
            &player,
            "glicko",
            &CamaRatingSystem::default(),
            &CamaOpenSkillSystem::new(),
        ),
        "1777"
    );
}

fn reminder_plan_by_delay(
    plan: Vec<BettingReminderPlan>,
) -> BTreeMap<i64, (BettingReminderType, bool)> {
    plan.into_iter()
        .map(|reminder| {
            (
                reminder.delay_seconds,
                (reminder.kind, reminder.is_final_warning),
            )
        })
        .collect()
}

#[test]
fn test_full_window_schedules_warnings_lastcall_and_close() {
    assert_eq!(
        reminder_plan_by_delay(betting_reminder_plan(900, &[600, 300], 60)),
        BTreeMap::from([
            (300, (BettingReminderType::Warning, false)),
            (600, (BettingReminderType::Warning, true)),
            (840, (BettingReminderType::LastCall, false)),
            (900, (BettingReminderType::Closed, false)),
        ])
    );
}

#[test]
fn test_short_window_skips_warnings_keeps_lastcall_and_close() {
    assert_eq!(
        reminder_plan_by_delay(betting_reminder_plan(200, &[600, 300], 60)),
        BTreeMap::from([
            (140, (BettingReminderType::LastCall, false)),
            (200, (BettingReminderType::Closed, false)),
        ])
    );
}

#[test]
fn test_tiny_window_only_schedules_close() {
    assert_eq!(
        reminder_plan_by_delay(betting_reminder_plan(30, &[600, 300], 60)),
        BTreeMap::from([(30, (BettingReminderType::Closed, false))])
    );
}

#[test]
fn test_already_closed_window_schedules_nothing() {
    assert!(betting_reminder_plan(-5, &[600, 300], 60).is_empty());
}

#[test]
fn test_last_call_offset_overrides_warning_at_same_offset() {
    assert_eq!(
        reminder_plan_by_delay(betting_reminder_plan(900, &[60, 300], 60)),
        BTreeMap::from([
            (600, (BettingReminderType::Warning, true)),
            (840, (BettingReminderType::LastCall, false)),
            (900, (BettingReminderType::Closed, false)),
        ])
    );
}

#[test]
fn test_only_smallest_warning_is_flagged_final() {
    let plan = reminder_plan_by_delay(betting_reminder_plan(900, &[600, 300], 60));
    assert_eq!(plan[&300], (BettingReminderType::Warning, false));
    assert_eq!(plan[&600], (BettingReminderType::Warning, true));
    assert!(!plan[&840].1);
    assert!(!plan[&900].1);
}

#[test]
fn test_single_warning_window_marks_it_final() {
    let plan = reminder_plan_by_delay(betting_reminder_plan(400, &[600, 300], 60));
    assert_eq!(plan[&100], (BettingReminderType::Warning, true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_reminder_tasks_are_owned_by_pending_match() {
    let fixture = MatchRuntimeFixture::new();
    let repository = PendingMatchRepository::new(fixture.database.path());
    let lock_until = unix_seconds() + 900;
    let state = PendingMatchState {
        bet_lock_until: Some(lock_until),
        lock_time: Some(lock_until),
        lobby_kind: Some("open".to_owned()),
        ..PendingMatchState::default()
    };
    let first = repository
        .create_pending_match(GUILD, &state)
        .expect("first reminder owner");
    let second = repository
        .create_pending_match(GUILD, &state)
        .expect("second reminder owner");
    fixture
        .provider
        .handler
        .schedule_betting_reminders(&first, false);
    fixture
        .provider
        .handler
        .schedule_betting_reminders(&second, false);
    fixture
        .provider
        .handler
        .cancel_betting_tasks(GUILD, Some(first.pending_match_id));
    let tasks = fixture
        .provider
        .handler
        .betting_tasks
        .lock()
        .expect("owned reminder tasks");
    assert!(!tasks.contains_key(&(GUILD, first.pending_match_id)));
    assert!(tasks.contains_key(&(GUILD, second.pending_match_id)));
    drop(tasks);
    abort_betting_tasks(&fixture, second.pending_match_id);
}

#[derive(Default)]
struct MatchReminderProbe {
    calls: Mutex<Vec<(i64, i64, i64, ReminderLobbyKind)>>,
}

#[derive(Default)]
struct MatchDebriefProbe {
    calls: Mutex<Vec<MatchPostMatchDebriefRequest>>,
}

#[async_trait]
impl MatchPostMatchDebriefPort for MatchDebriefProbe {
    async fn on_post_match_debrief(
        &self,
        request: MatchPostMatchDebriefRequest,
    ) -> Result<(), String> {
        self.calls.lock().expect("debrief probe").push(request);
        Ok(())
    }
}

#[derive(Default)]
struct MatchHookOrderProbe {
    calls: Mutex<Vec<&'static str>>,
    easter_eggs: Mutex<Vec<MatchEasterEggRequest>>,
}

#[async_trait]
impl MatchPostMatchDebriefPort for MatchHookOrderProbe {
    async fn on_bet_settlement(&self, _request: MatchBetSettlementRequest) -> Result<(), String> {
        self.calls
            .lock()
            .expect("settlement probe")
            .push("settlement");
        Ok(())
    }

    async fn on_match_easter_eggs(&self, request: MatchEasterEggRequest) -> Result<(), String> {
        self.calls
            .lock()
            .expect("easter-egg order probe")
            .push("easter_eggs");
        self.easter_eggs
            .lock()
            .expect("easter-egg request probe")
            .push(request);
        Ok(())
    }

    async fn on_post_match_debrief(
        &self,
        _request: MatchPostMatchDebriefRequest,
    ) -> Result<(), String> {
        self.calls.lock().expect("debrief probe").push("debrief");
        Ok(())
    }
}

#[test]
fn match_easter_egg_collector_uses_exact_python_milestones_and_new_streak_records() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(6, false);
    let milestone_totals = [10_i64, 50, 100, 200, 500];
    let connection = Connection::open(fixture.database.path()).expect("open milestone fixture");
    for (discord_id, total_games) in player_ids[..5].iter().zip(milestone_totals) {
        connection
            .execute(
                "UPDATE players SET wins=?1, losses=0
                 WHERE guild_id=?2 AND discord_id=?3",
                params![total_games, GUILD, discord_id],
            )
            .expect("seed exact milestone total");
    }
    connection
        .execute(
            "UPDATE players SET wins=10, losses=1, personal_best_win_streak=4
             WHERE guild_id=?1 AND discord_id=?2",
            params![GUILD, player_ids[5]],
        )
        .expect("seed non-milestone streak player");
    connection
        .execute(
            "INSERT INTO player_pairings(
                 guild_id,player1_id,player2_id,games_against,player1_wins_against
             ) VALUES(?1,?2,?3,10,8)",
            params![GUILD, player_ids[0], player_ids[1]],
        )
        .expect("seed committed rivalry aggregate");
    drop(connection);

    let streak_data = BTreeMap::from([(
        player_ids[5],
        StreakAdjustment {
            streak_length: 5,
            multiplier: 1.6,
        },
    )]);
    let request = fixture
        .provider
        .handler
        .collect_match_easter_eggs(
            &player_ids,
            &player_ids[..1],
            &player_ids[1..2],
            &[player_ids[5]],
            &streak_data,
            None,
            GUILD,
            92_101,
            Some(77_001),
        )
        .expect("collect production Match Easter eggs");

    assert_eq!(
        request.games_milestones,
        player_ids[..5]
            .iter()
            .zip(milestone_totals)
            .map(|(&discord_id, games_played)| GamesMilestone {
                discord_id,
                games_played,
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(
        request.win_streak_records,
        vec![WinStreakRecord {
            discord_id: player_ids[5],
            current_streak: 5,
            previous_best: 4,
        }]
    );
    assert_eq!(
        request.rivalries_detected,
        vec![RivalryRecord {
            player1_id: player_ids[0],
            player2_id: player_ids[1],
            games_against: 10,
            winrate_vs: 80.0,
        }]
    );
    assert_eq!(
        PlayerRepository::new(fixture.database.path())
            .get_by_id(player_ids[5], Some(GUILD))
            .expect("read pre-commit personal best")
            .expect("streak player")
            .personal_best_win_streak,
        4
    );
    fixture
        .provider
        .handler
        .apply_easter_streak_records(GUILD, &request.win_streak_records)
        .expect("apply committed personal best");
    assert_eq!(
        PlayerRepository::new(fixture.database.path())
            .get_by_id(player_ids[5], Some(GUILD))
            .expect("read committed personal best")
            .expect("streak player")
            .personal_best_win_streak,
        5
    );
}

#[test]
fn match_easter_egg_collector_keeps_milestones_when_pairing_lookup_fails() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(2, false);
    Connection::open(fixture.database.path())
        .expect("open fail-soft pairing fixture")
        .execute(
            "UPDATE players SET wins=10, losses=0
             WHERE guild_id=?1 AND discord_id IN (?2,?3)",
            params![GUILD, player_ids[0], player_ids[1]],
        )
        .expect("seed milestone players");
    Connection::open(fixture.database.path())
        .expect("reopen fail-soft pairing fixture")
        .execute_batch("DROP TABLE player_pairings")
        .expect("remove pairing table to force lookup failure");

    let request = fixture
        .provider
        .handler
        .collect_match_easter_eggs(
            &player_ids,
            &player_ids[..1],
            &player_ids[1..2],
            &[],
            &BTreeMap::new(),
            None,
            GUILD,
            92_103,
            Some(77_001),
        )
        .expect("pairing failure must not fail committed match presentation");
    assert_eq!(request.games_milestones.len(), 2);
    assert!(request.win_streak_records.is_empty());
    assert!(request.rivalries_detected.is_empty());
}

#[test]
fn committed_easter_streak_payload_survives_crash_retry_after_best_update() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let winner = pending.state.radiant_team_ids[0];
    let streak_data = BTreeMap::from([(
        winner,
        StreakAdjustment {
            streak_length: 5,
            multiplier: 1.6,
        },
    )]);
    let first = fixture
        .provider
        .handler
        .collect_match_easter_eggs(
            &pending
                .state
                .participant_ids()
                .into_iter()
                .collect::<Vec<_>>(),
            &pending.state.radiant_team_ids,
            &pending.state.dire_team_ids,
            &[winner],
            &streak_data,
            None,
            GUILD,
            92_104,
            Some(77_001),
        )
        .expect("first committed Easter payload");
    assert_eq!(first.win_streak_records.len(), 1);
    assert_eq!(first.win_streak_records[0].previous_best, 0);
    fixture
        .provider
        .handler
        .pending
        .mutate_pending_match(GUILD, pending.pending_match_id, |state| {
            persist_easter_streaks_in_state(state, &first.win_streak_records);
        })
        .expect("persist committed Easter payload")
        .expect("pending row for committed Easter payload");
    fixture
        .provider
        .handler
        .apply_easter_streak_records(GUILD, &first.win_streak_records)
        .expect("apply first committed personal best");

    let retry_pending = PendingMatchRepository::new(fixture.database.path())
        .pending_match(GUILD, pending.pending_match_id)
        .expect("reload pending match")
        .expect("pending retry row");
    let persisted = persisted_easter_streaks(&retry_pending.state)
        .expect("decode committed Easter payload")
        .expect("streak payload");
    let retry = fixture
        .provider
        .handler
        .collect_match_easter_eggs(
            &retry_pending
                .state
                .participant_ids()
                .into_iter()
                .collect::<Vec<_>>(),
            &retry_pending.state.radiant_team_ids,
            &retry_pending.state.dire_team_ids,
            &[winner],
            &streak_data,
            Some(&persisted),
            GUILD,
            92_104,
            Some(77_001),
        )
        .expect("reconstruct Easter payload after crash");
    assert_eq!(retry.win_streak_records, first.win_streak_records);
    assert_eq!(retry.win_streak_records[0].previous_best, 0);
}

#[test]
fn easter_streak_persistence_failure_leaves_personal_best_unmutated() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let winner = pending.state.radiant_team_ids[0];
    let streak_data = BTreeMap::from([(
        winner,
        StreakAdjustment {
            streak_length: 5,
            multiplier: 1.6,
        },
    )]);
    let first = fixture
        .provider
        .handler
        .collect_match_easter_eggs(
            &pending
                .state
                .participant_ids()
                .into_iter()
                .collect::<Vec<_>>(),
            &pending.state.radiant_team_ids,
            &pending.state.dire_team_ids,
            &[winner],
            &streak_data,
            None,
            GUILD,
            92_105,
            Some(77_001),
        )
        .expect("compute streak payload before forced persistence failure");
    assert_eq!(first.win_streak_records[0].previous_best, 0);
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .delete_pending_match(GUILD, pending.pending_match_id)
            .expect("delete pending row to force persistence failure")
    );
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .mutate_pending_match(GUILD, pending.pending_match_id, |state| {
                persist_easter_streaks_in_state(state, &first.win_streak_records);
            })
            .expect("forced persistence operation")
            .is_none()
    );
    assert_eq!(
        PlayerRepository::new(fixture.database.path())
            .get_personal_best_win_streak(winner, Some(GUILD))
            .expect("read untouched personal best"),
        0
    );
}

#[test]
fn persisted_easter_streak_payload_survives_best_update_failure() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let winner = pending.state.radiant_team_ids[0];
    let records = vec![WinStreakRecord {
        discord_id: winner,
        current_streak: 5,
        previous_best: 0,
    }];
    fixture
        .provider
        .handler
        .pending
        .mutate_pending_match(GUILD, pending.pending_match_id, |state| {
            persist_easter_streaks_in_state(state, &records);
        })
        .expect("persist streak payload before forced update failure")
        .expect("pending row for forced update failure");
    Connection::open(fixture.database.path())
        .expect("open forced update failure database")
        .execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE players;")
        .expect("remove player table to force update failure");
    assert!(
        fixture
            .provider
            .handler
            .apply_easter_streak_records(GUILD, &records)
            .is_err()
    );
    let retry_pending = PendingMatchRepository::new(fixture.database.path())
        .pending_match(GUILD, pending.pending_match_id)
        .expect("reload payload after update failure")
        .expect("pending payload retained");
    assert_eq!(
        persisted_easter_streaks(&retry_pending.state)
            .expect("decode retained payload")
            .expect("retained streak records"),
        records
    );
}

#[tokio::test(flavor = "current_thread")]
async fn post_match_hooks_deliver_settlement_then_easter_eggs_then_debrief() {
    let fixture = MatchRuntimeFixture::new();
    let probe = Arc::new(MatchHookOrderProbe::default());
    fixture
        .provider
        .attach_post_match_debrief(Arc::clone(&probe) as Arc<dyn MatchPostMatchDebriefPort>)
        .expect("attach match hook order probe");
    fixture
        .provider
        .handler
        .emit_post_match_hooks(
            Some(MatchBetSettlementRequest {
                guild_id: GUILD,
                match_id: 92_102,
                channel_id: Some(77_001),
                participants: Vec::new(),
            }),
            Some(MatchEasterEggRequest {
                guild_id: GUILD,
                match_id: 92_102,
                channel_id: Some(77_001),
                games_milestones: vec![GamesMilestone {
                    discord_id: WINNER,
                    games_played: 10,
                }],
                win_streak_records: vec![WinStreakRecord {
                    discord_id: SKIMMER,
                    current_streak: 5,
                    previous_best: 4,
                }],
                rivalries_detected: vec![RivalryRecord {
                    player1_id: WINNER,
                    player2_id: SKIMMER,
                    games_against: 10,
                    winrate_vs: 80.0,
                }],
            }),
            Some(MatchPostMatchDebriefRequest {
                guild_id: GUILD,
                match_id: 92_102,
                channel_id: Some(77_001),
                winner_id: Some(WINNER),
                loser_id: Some(SKIMMER),
                payout: 0,
                loss: 0,
                leverage: 1,
                rating_change: None,
                expected_win_prob: None,
            }),
        )
        .await;

    assert_eq!(
        probe.calls.lock().expect("hook order calls").as_slice(),
        ["settlement", "easter_eggs", "debrief"]
    );
    let requests = probe.easter_eggs.lock().expect("hook request calls");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].games_milestones[0].games_played, 10);
    assert_eq!(requests[0].win_streak_records[0].current_streak, 5);
}

#[tokio::test(flavor = "current_thread")]
async fn post_match_debrief_reads_rating_history_and_preserves_openskill_gain() {
    let fixture = MatchRuntimeFixture::new();
    let match_id = 92_001;
    let first_winner = 72_001;
    let notable_winner = 72_002;
    let connection = Connection::open(fixture.database.path()).expect("open debrief database");
    connection
        .execute_batch("PRAGMA foreign_keys=OFF")
        .expect("disable legacy foreign keys");
    connection
        .execute_batch(
            "INSERT INTO rating_history(
                 discord_id,guild_id,match_id,rating,rating_before,
                 expected_team_win_prob,os_mu_before,os_mu_after,won
             ) VALUES
                 (72001,82001,92001,1520.0,1500.0,0.55,40.0,40.2,1),
                 (72002,82001,92001,1505.0,1500.0,0.55,40.0,41.0,1)",
        )
        .expect("insert debrief rating history");
    drop(connection);

    let probe = Arc::new(MatchDebriefProbe::default());
    fixture
        .provider
        .attach_post_match_debrief(Arc::clone(&probe) as Arc<dyn MatchPostMatchDebriefPort>)
        .expect("attach debrief port");
    let request = fixture
        .provider
        .handler
        .build_post_match_debrief(
            match_id,
            GUILD,
            &[first_winner, notable_winner],
            &SettlementResult::default(),
            None,
        )
        .expect("notable winner request");
    assert_eq!(request.winner_id, Some(notable_winner));
    assert_eq!(request.rating_change, Some(50.0));
    assert_eq!(request.expected_win_prob, Some(0.55));
    fixture
        .provider
        .handler
        .emit_post_match_debrief(Some(request))
        .await;
    let calls = probe.calls.lock().expect("debrief calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].match_id, match_id);
}

#[tokio::test(flavor = "current_thread")]
async fn wager_refresh_adapter_reloads_pending_match_and_reports_edits() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture =
        MatchRuntimeFixture::new_with_discord(Arc::clone(&discord) as Arc<dyn DiscordTransport>);
    let pending = PendingMatchRepository::new(fixture.database.path())
        .create_pending_match(
            GUILD,
            &PendingMatchState {
                shuffle_channel_id: Some(70_001),
                shuffle_message_id: Some(80_001),
                shuffle_timestamp: Some(unix_seconds()),
                lobby_kind: Some("open".to_owned()),
                ..PendingMatchState::default()
            },
        )
        .expect("pending wager refresh match");

    let report = fixture
        .provider
        .refresh_wager_message(GUILD, pending.pending_match_id)
        .await
        .expect("refresh pending wager message");
    assert_eq!(report.attempted, 1);
    assert_eq!(report.refreshed, 1);
    assert!(report.failures.is_empty());
    assert_eq!(discord.message_edits().len(), 1);
}

#[async_trait]
impl MatchReminderPort for MatchReminderProbe {
    async fn notify_betting(
        &self,
        guild_id: i64,
        bet_lock_until: i64,
        pending_match_id: i64,
        lobby_kind: ReminderLobbyKind,
    ) -> Result<ReminderDeliveryReport, String> {
        self.calls.lock().expect("reminder probe").push((
            guild_id,
            bet_lock_until,
            pending_match_id,
            lobby_kind,
        ));
        Ok(ReminderDeliveryReport::default())
    }
}

#[derive(Default)]
struct ReminderFlavorProbe {
    requests: Mutex<Vec<FlavorRequest>>,
}

impl BettingFlavorPort for ReminderFlavorProbe {
    fn generate(&self, request: &FlavorRequest) -> Result<Option<String>, String> {
        self.requests
            .lock()
            .expect("reminder flavor requests")
            .push(request.clone());
        Ok(Some("REMINDER FLAVOR".to_owned()))
    }
}

struct ProductionFlavorProvider {
    line: String,
    requests: Mutex<Vec<ProviderRequest>>,
}

impl ProductionFlavorProvider {
    fn new(line: &str) -> Arc<Self> {
        Arc::new(Self {
            line: line.to_owned(),
            requests: Mutex::new(Vec::new()),
        })
    }
}

impl AiProvider for ProductionFlavorProvider {
    fn complete(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        self.requests
            .lock()
            .expect("production flavor requests")
            .push(request);
        Ok(ProviderResponse {
            tool_calls: vec![ToolCall {
                name: FLAVOR_TOOL_NAME.to_owned(),
                arguments: BTreeMap::from([("comment".to_owned(), Value::Text(self.line.clone()))]),
            }],
            ..ProviderResponse::default()
        })
    }
}

struct ProductionFlavorLoader(Arc<ProductionFlavorProvider>);

impl ProviderLoader for ProductionFlavorLoader {
    fn load(&self) -> Result<Arc<dyn AiProvider>, ProviderError> {
        Ok(self.0.clone())
    }
}

fn production_flavor_ai(provider: Arc<ProductionFlavorProvider>) -> Arc<AIService> {
    let config = ProviderConfig::new(
        "test/betting-flavor",
        ProviderCredential::new("test-key").expect("test AI credential"),
        Duration::from_secs(1),
        500,
    )
    .expect("test AI provider config");
    Arc::new(AIService::new(
        config,
        Arc::new(ProductionFlavorLoader(provider)),
    ))
}

fn production_flavor_request(kind: FlavorKind, guild_id: i64) -> FlavorRequest {
    FlavorRequest {
        kind,
        guild_id: Some(guild_id),
        standings: "Radiant: 100 | Dire: 800".to_owned(),
        seconds_left: 60,
        leader_team: Some("dire".to_owned()),
        leader_amount: Some(800),
        leader_discord_id: Some(81_105),
        underdog_side: None,
    }
}

#[test]
fn production_betting_flavor_without_ai_uses_static_fallback() {
    let fixture = MatchRuntimeFixture::new();
    let flavor =
        production_betting_flavor(fixture.database.path(), &production_test_config(), None);
    let output = flavor
        .generate(&production_flavor_request(FlavorKind::LastCall, GUILD))
        .expect("static betting flavor");
    assert!(
        cama_app::ai_services::BETTING_LAST_CALL_EXAMPLES
            .contains(&output.as_deref().expect("fallback line")),
        "no-AI composition must preserve the visible Python fallback"
    );
}

#[test]
fn production_betting_flavor_loads_sqlite_leader_context_including_zero_win_rate() {
    let fixture = MatchRuntimeFixture::new();
    let _pending = fixture.pending(unix_seconds() + 900);
    let settled_match_id = 500_001_i64;
    let connection = Connection::open(fixture.database.path()).expect("open flavor database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable legacy fixture foreign keys");
    connection
        .execute(
            "INSERT INTO matches(
                 match_id,team1_players,team2_players,winning_team,guild_id
             ) VALUES(?1,'[]','[]',2,?2)",
            params![settled_match_id, GUILD],
        )
        .expect("seed settled flavor match");
    connection
        .execute(
            "INSERT INTO bets(
                 guild_id,match_id,discord_id,team_bet_on,amount,bet_time,
                 leverage,payout,is_blind
             ) VALUES(?1,?2,?3,'radiant',800,?4,1,0,0)",
            params![GUILD, settled_match_id, 81_105_i64, unix_seconds()],
        )
        .expect("seed losing leader bet");
    drop(connection);

    let mut config = production_test_config();
    config.values.ai_features_enabled = true;
    let provider = ProductionFlavorProvider::new("PRODUCTION LEADER FLAVOR");
    let flavor = production_betting_flavor(
        fixture.database.path(),
        &config,
        Some(production_flavor_ai(Arc::clone(&provider))),
    );
    let output = flavor
        .generate(&production_flavor_request(FlavorKind::LastCall, GUILD))
        .expect("production betting flavor")
        .expect("AI line");
    assert_eq!(output, "PRODUCTION LEADER FLAVOR");
    let requests = provider
        .requests
        .lock()
        .expect("read production flavor request");
    assert_eq!(requests.len(), 1);
    let prompt = requests[0]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(prompt.contains("Biggest bettor so far: ready-81105"));
    assert!(prompt.contains("Their bet win rate: 0%"));
    assert!(prompt.contains("800 on dire"));
}

#[test]
fn production_betting_flavor_respects_guild_ai_disabled_without_provider_call() {
    let fixture = MatchRuntimeFixture::new();
    GuildConfigRepository::new(fixture.database.path(), true)
        .set_ai_enabled(GUILD, false)
        .expect("disable guild AI");
    let mut config = production_test_config();
    config.values.ai_features_enabled = true;
    let provider = ProductionFlavorProvider::new("SHOULD NOT BE USED");
    let flavor = production_betting_flavor(
        fixture.database.path(),
        &config,
        Some(production_flavor_ai(Arc::clone(&provider))),
    );
    let output = flavor
        .generate(&production_flavor_request(FlavorKind::LastCall, GUILD))
        .expect("disabled guild fallback")
        .expect("static fallback line");
    assert!(cama_app::ai_services::BETTING_LAST_CALL_EXAMPLES.contains(&output.as_str()));
    assert!(
        provider
            .requests
            .lock()
            .expect("provider requests")
            .is_empty(),
        "disabled guild must not call the AI provider"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_subscriber_notification_receives_match_and_lobby_identity() {
    let fixture = MatchRuntimeFixture::new();
    let repository = PendingMatchRepository::new(fixture.database.path());
    let lock_until = unix_seconds() + 900;
    let pending = repository
        .create_pending_match(
            GUILD,
            &PendingMatchState {
                bet_lock_until: Some(lock_until),
                lock_time: Some(lock_until),
                lobby_kind: Some("lowskill".to_owned()),
                ..PendingMatchState::default()
            },
        )
        .expect("subscriber reminder pending match");
    let probe = Arc::new(MatchReminderProbe::default());
    let port: Arc<dyn MatchReminderPort> = probe.clone();
    assert!(fixture.provider.handler.betting_reminders.set(port).is_ok());
    fixture
        .provider
        .handler
        .schedule_betting_reminders(&pending, true);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !probe.calls.lock().expect("reminder probe").is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("subscriber delivery attempted");
    assert_eq!(
        *probe.calls.lock().expect("reminder probe"),
        vec![(
            GUILD,
            lock_until,
            pending.pending_match_id,
            ReminderLobbyKind::LowSkill,
        )]
    );
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "current_thread")]
async fn final_warning_uses_scoped_underdog_mentions_and_thread_reply_parent() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture =
        MatchRuntimeFixture::new_with_discord(Arc::clone(&discord) as Arc<dyn DiscordTransport>);
    let pending = fixture.pending(unix_seconds() + 900);
    let mut state = pending.state.clone();
    state.origin_channel_id = Some(77_001);
    state.shuffle_channel_id = Some(77_001);
    state.shuffle_message_id = Some(77_002);
    state.thread_shuffle_thread_id = Some(77_003);
    state.thread_shuffle_message_id = Some(77_004);
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .update_pending_match(GUILD, pending.pending_match_id, &state)
            .expect("persist reminder message targets")
    );
    let connection = Connection::open(fixture.database.path()).expect("open reminder database");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=1000
             WHERE guild_id=?1 AND discord_id IN (?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                GUILD, 81_100_i64, 81_101_i64, 81_102_i64, 81_103_i64, 81_104_i64, 81_105_i64,
                81_106_i64, 81_107_i64, 81_108_i64, 81_109_i64
            ],
        )
        .expect("seed reminder balances");
    drop(connection);
    let bets = BettingServiceRepository::new(fixture.database.path());
    bets.place_bet_atomic(PlaceBetRequest {
        guild_id: Some(GUILD),
        pending_match_id: pending.pending_match_id,
        discord_id: 81_100,
        team: BettingTeam::Radiant,
        amount: 100,
        bet_time: unix_seconds(),
        leverage: 1,
        max_debt: 500,
        is_blind: false,
        odds_at_placement: None,
    })
    .expect("place underdog wager");
    bets.place_bet_atomic(PlaceBetRequest {
        guild_id: Some(GUILD),
        pending_match_id: pending.pending_match_id,
        discord_id: 81_105,
        team: BettingTeam::Dire,
        amount: 500,
        bet_time: unix_seconds(),
        leverage: 1,
        max_debt: 500,
        is_blind: false,
        odds_at_placement: None,
    })
    .expect("place favorite wager");
    let flavor = Arc::new(ReminderFlavorProbe::default());
    fixture
        .provider
        .attach_betting_flavor(flavor.clone())
        .expect("attach reminder flavor");
    fixture
        .provider
        .handler
        .run_betting_reminder(
            GUILD,
            pending.pending_match_id,
            state.bet_lock_until.expect("lock time"),
            BettingReminderType::Warning,
            true,
        )
        .await;

    let replies = discord.thread_replies();
    assert_eq!(replies.len(), 1);
    assert_eq!((replies[0].0, replies[0].1), (77_003, 77_004));
    assert_eq!(
        replies[0].2.allowed_mentions,
        DiscordAllowedMentions::Users(BTreeSet::from([81_100_u64, 81_101, 81_102, 81_103, 81_104]))
    );
    assert!(replies[0].2.response.content.contains("REMINDER FLAVOR"));
    assert!(replies[0].2.response.content.contains("<@81100>"));
    let requests = flavor
        .requests
        .lock()
        .expect("read reminder flavor requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].underdog_side,
        Some(cama_app::betting_reminder_messaging::UnderdogSide::Radiant)
    );
    assert_eq!(requests[0].leader_discord_id, Some(81_105));
    assert_eq!(requests[0].leader_amount, Some(500));
}

fn abort_betting_tasks(fixture: &MatchRuntimeFixture, pending_match_id: i64) {
    if let Some(tasks) = fixture
        .provider
        .handler
        .betting_tasks
        .lock()
        .expect("betting task registry")
        .remove(&(GUILD, pending_match_id))
    {
        for task in tasks {
            task.abort();
        }
    }
}

struct NoReadyMembers;

#[async_trait]
impl crate::gateway_events::GuildMemberPageSource for NoReadyMembers {
    async fn fetch_page(
        &self,
        _guild_id: u64,
        _after: Option<u64>,
        _limit: u64,
    ) -> Result<Vec<crate::gateway_events::GatewayMember>, String> {
        Ok(Vec::new())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ready_recovery_rearms_uncommitted_pending_match_from_absolute_deadline() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let context = ReadyRecoveryContext::new(
        Arc::<[u64]>::from([u64::try_from(GUILD).expect("guild snowflake")]),
        Arc::new(NoReadyMembers),
    );
    let report = fixture
        .provider
        .gateway_observer
        .ready_recovery(context)
        .await;
    assert_eq!(report.guilds_refreshed, 1);
    assert!(report.failures.is_empty());
    let tasks = fixture
        .provider
        .handler
        .betting_tasks
        .lock()
        .expect("betting task registry");
    let scheduled = tasks
        .get(&(GUILD, pending.pending_match_id))
        .expect("recovered betting tasks");
    assert_eq!(scheduled.len(), 3);
    for task in scheduled {
        task.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ready_recovery_reenters_committed_match_money_exactly_once_then_clears_pending() {
    let fixture = MatchRuntimeFixture::new();
    let mut pending = fixture.pending(unix_seconds() + 120);
    let players = PlayerRepository::new(fixture.database.path());
    for discord_id in [81_110_i64, 81_111] {
        let mut player = NewPlayer::new(discord_id, format!("excluded-{discord_id}"), Some(GUILD));
        player.initial_mmr = Some(4_000);
        player.glicko_rating = Some(1_500.0);
        player.glicko_rd = Some(200.0);
        player.glicko_volatility = Some(0.06);
        player.os_mu = Some(CamaOpenSkillSystem::DEFAULT_MU);
        player.os_sigma = Some(CamaOpenSkillSystem::DEFAULT_SIGMA);
        players.add(&player).expect("insert excluded player");
    }
    Connection::open(fixture.database.path())
        .expect("seed exclusion counters")
        .execute(
            "UPDATE players SET exclusion_count=2
             WHERE guild_id=?1 AND discord_id BETWEEN 81100 AND 81109",
            [GUILD],
        )
        .expect("seed participant exclusion counters");
    let avoid = SoftAvoidRepository::new(fixture.database.path())
        .create_or_reactivate_avoid(
            Some(GUILD),
            pending.state.radiant_team_ids[0],
            pending.state.dire_team_ids[0],
            3,
        )
        .expect("seed effective avoid");
    pending.state.excluded_player_ids = vec![81_110, 81_111];
    pending.state.exclusion_updates_deferred = true;
    pending.state.full_exclusion_increment_ids = vec![81_110];
    pending.state.half_exclusion_increment_ids = vec![81_111];
    pending.state.effective_avoid_ids = vec![avoid.id];
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .update_pending_match(GUILD, pending.pending_match_id, &pending.state)
            .expect("persist match recovery policy state")
    );
    assert_eq!(
        SoftAvoidRepository::new(fixture.database.path())
            .get_active_avoids_for_players(
                Some(GUILD),
                &[
                    pending.state.radiant_team_ids[0],
                    pending.state.dire_team_ids[0]
                ],
            )
            .expect("read avoid before record")[0]
            .games_remaining,
        3
    );
    let recorded = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("commit match before simulated crash");
    let connection = Connection::open(fixture.database.path()).expect("snapshot committed match");
    let before: (i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT
                 (SELECT COALESCE(SUM(jopacoin_balance),0) FROM players WHERE guild_id=?1),
                 (SELECT COALESCE(SUM(wins),0) FROM players WHERE guild_id=?1),
                 (SELECT COALESCE(SUM(losses),0) FROM players WHERE guild_id=?1),
                 (SELECT COUNT(*) FROM economy_ledger_entries WHERE guild_id=?1),
                 (SELECT COUNT(*) FROM player_pairings WHERE guild_id=?1)",
            [GUILD],
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
        .expect("read committed match snapshot");
    drop(connection);
    assert_eq!((before.1, before.2), (5, 5));
    assert_eq!(before.4, 45);
    let connection = Connection::open(fixture.database.path()).expect("inspect policy effects");
    let policy_before: (i64, i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT
                 (SELECT games_remaining FROM soft_avoids WHERE id=?1),
                 (SELECT MIN(exclusion_count) FROM players
                  WHERE guild_id=?2 AND discord_id BETWEEN 81100 AND 81109),
                 (SELECT exclusion_count FROM players WHERE guild_id=?2 AND discord_id=81110),
                 (SELECT exclusion_count FROM players WHERE guild_id=?2 AND discord_id=81111),
                 (SELECT COUNT(*) FROM players
                  WHERE guild_id=?2 AND last_match_date IS NOT NULL),
                 (SELECT COUNT(*) FROM players
                  WHERE guild_id=?2 AND glicko_rating != 1500.0)",
            params![avoid.id, GUILD],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("read committed policy effects");
    assert_eq!(policy_before, (2, 1, 11, 6, 10, 10));
    drop(connection);

    let context = ReadyRecoveryContext::new(
        Arc::<[u64]>::from([u64::try_from(GUILD).expect("guild snowflake")]),
        Arc::new(NoReadyMembers),
    );
    let report = fixture
        .provider
        .gateway_observer
        .ready_recovery(context)
        .await;
    assert_eq!(report.guilds_refreshed, 1);
    assert!(report.failures.is_empty());
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read recovered pending")
            .is_none()
    );
    let connection = Connection::open(fixture.database.path()).expect("inspect READY recovery");
    let after: (i64, i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT
                 (SELECT COALESCE(SUM(jopacoin_balance),0) FROM players WHERE guild_id=?1),
                 (SELECT COALESCE(SUM(wins),0) FROM players WHERE guild_id=?1),
                 (SELECT COALESCE(SUM(losses),0) FROM players WHERE guild_id=?1),
                 (SELECT COUNT(*) FROM economy_ledger_entries WHERE guild_id=?1),
                 (SELECT COUNT(*) FROM player_pairings WHERE guild_id=?1),
                 (SELECT COUNT(*) FROM matches WHERE guild_id=?1)",
            [GUILD],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("read recovered match snapshot");
    assert_eq!((after.0, after.1, after.2, after.3, after.4), before);
    assert_eq!(after.5, 1);
    let policy_after: (i64, i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT
                 (SELECT games_remaining FROM soft_avoids WHERE id=?1),
                 (SELECT MIN(exclusion_count) FROM players
                  WHERE guild_id=?2 AND discord_id BETWEEN 81100 AND 81109),
                 (SELECT exclusion_count FROM players WHERE guild_id=?2 AND discord_id=81110),
                 (SELECT exclusion_count FROM players WHERE guild_id=?2 AND discord_id=81111),
                 (SELECT COUNT(*) FROM players
                  WHERE guild_id=?2 AND last_match_date IS NOT NULL),
                 (SELECT COUNT(*) FROM players
                  WHERE guild_id=?2 AND glicko_rating != 1500.0)",
            params![avoid.id, GUILD],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("read recovered policy effects");
    assert_eq!(policy_after, policy_before);
    assert_eq!(
        MatchRepository::new(fixture.database.path())
            .match_id_for_pending_match(GUILD, pending.pending_match_id)
            .expect("read recovered match id"),
        Some(recorded.match_id)
    );
}

#[test]
fn record_retry_recovers_settled_bet_snapshot_without_double_pay() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let winning_bettor = 82_201_i64;
    let losing_bettor = 82_202_i64;
    let players = PlayerRepository::new(fixture.database.path());
    for discord_id in [winning_bettor, losing_bettor] {
        let mut player = NewPlayer::new(discord_id, format!("bettor-{discord_id}"), Some(GUILD));
        player.initial_mmr = Some(4_000);
        player.glicko_rating = Some(1_500.0);
        player.glicko_rd = Some(200.0);
        player.glicko_volatility = Some(0.06);
        player.os_mu = Some(CamaOpenSkillSystem::DEFAULT_MU);
        player.os_sigma = Some(CamaOpenSkillSystem::DEFAULT_SIGMA);
        players.add(&player).expect("insert betting spectator");
    }
    let betting = BettingServiceRepository::new(fixture.database.path());
    for (discord_id, team) in [
        (winning_bettor, BettingTeam::Radiant),
        (losing_bettor, BettingTeam::Dire),
    ] {
        Connection::open(fixture.database.path())
            .expect("open bettor balance")
            .execute(
                "UPDATE players SET jopacoin_balance=50
                 WHERE guild_id=?1 AND discord_id=?2",
                params![GUILD, discord_id],
            )
            .expect("seed bettor balance");
        betting
            .place_bet_atomic(PlaceBetRequest {
                guild_id: Some(GUILD),
                pending_match_id: pending.pending_match_id,
                discord_id,
                team,
                amount: 5,
                bet_time: pending.state.shuffle_timestamp.unwrap_or_default(),
                leverage: 1,
                max_debt: 500,
                is_blind: false,
                odds_at_placement: None,
            })
            .expect("place spectator bet");
    }

    let first = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("record match with bets");
    let connection =
        Connection::open(fixture.database.path()).expect("inspect first betting settlement");
    let after_first = connection
        .prepare(
            "SELECT discord_id,jopacoin_balance FROM players
             WHERE guild_id=?1 AND discord_id IN (?2,?3)
             ORDER BY discord_id",
        )
        .expect("prepare first bettor balances")
        .query_map(params![GUILD, winning_bettor, losing_bettor], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("read first bettor balances")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect first bettor balances");
    let first_snapshot = MatchRepository::new(fixture.database.path())
        .get_match(first.match_id, Some(GUILD))
        .expect("read first match snapshot")
        .expect("first match");
    assert_eq!(first_snapshot.jc_changes[&winning_bettor]["bet"], 5);
    assert_eq!(first_snapshot.jc_changes[&losing_bettor]["bet"], -5);

    let retry = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("recover already-settled bets");
    let connection =
        Connection::open(fixture.database.path()).expect("inspect retry betting settlement");
    let after_retry = connection
        .prepare(
            "SELECT discord_id,jopacoin_balance FROM players
             WHERE guild_id=?1 AND discord_id IN (?2,?3)
             ORDER BY discord_id",
        )
        .expect("prepare retry bettor balances")
        .query_map(params![GUILD, winning_bettor, losing_bettor], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("read retry bettor balances")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect retry bettor balances");
    let retry_snapshot = MatchRepository::new(fixture.database.path())
        .get_match(retry.match_id, Some(GUILD))
        .expect("read retry match snapshot")
        .expect("retry match");
    assert_eq!(retry.match_id, first.match_id);
    assert_eq!(after_retry, after_first);
    assert_eq!(retry_snapshot.jc_changes, first_snapshot.jc_changes);
    assert_eq!(
        Connection::open(fixture.database.path())
            .expect("inspect settled bet rows")
            .query_row(
                "SELECT COUNT(*) FROM bets
                 WHERE guild_id=?1 AND match_id=?2",
                params![GUILD, first.match_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("count settled bets"),
        2
    );
}

fn recorded_match_id_for_pending(fixture: &MatchRuntimeFixture, pending_match_id: i64) -> i64 {
    MatchRepository::new(fixture.database.path())
        .match_id_for_pending_match(GUILD, pending_match_id)
        .expect("read match for pending ID")
        .expect("core match committed before post-core failure")
}

fn assert_bonus_phase_not_started(fixture: &MatchRuntimeFixture, match_id: i64) {
    let state = Connection::open(fixture.database.path())
        .expect("inspect pre-bonus failure state")
        .query_row(
            "SELECT COALESCE(bonuses_paid,0),
                    (SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND related_id=?2
                       AND source IN ('match_participation','match_win_bonus'))
             FROM matches WHERE guild_id=?1 AND match_id=?2",
            params![GUILD, match_id.to_string()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .expect("read pre-bonus failure state");
    assert_eq!(state, (0, 0));
}

fn assert_participant_rewards_paid_once(fixture: &MatchRuntimeFixture, match_id: i64) {
    let state = Connection::open(fixture.database.path())
        .expect("inspect recovered participant rewards")
        .query_row(
            "SELECT COUNT(*),COALESCE(SUM(delta),0)
             FROM economy_ledger_entries
             WHERE guild_id=?1 AND related_id=?2
               AND source IN ('match_participation','match_win_bonus')",
            params![GUILD, match_id.to_string()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .expect("read recovered participant rewards");
    assert_eq!(state, (10, 110));
}

#[test]
fn snapshot_jc_changes_read_failure_does_not_consume_bonus_claim() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    Connection::open(fixture.database.path())
        .expect("install malformed snapshot trigger")
        .execute_batch(
            "CREATE TRIGGER corrupt_new_match_snapshot
             AFTER INSERT ON matches BEGIN
               UPDATE matches SET jc_changes='{not-json' WHERE match_id=NEW.match_id;
             END;",
        )
        .expect("install malformed snapshot trigger");

    let error = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect_err("malformed aggregate snapshot stops before bonuses");
    assert!(error.contains("JSON") || error.contains("json"));
    let match_id = recorded_match_id_for_pending(&fixture, pending.pending_match_id);
    assert_bonus_phase_not_started(&fixture, match_id);

    Connection::open(fixture.database.path())
        .expect("repair malformed snapshot")
        .execute_batch(
            "DROP TRIGGER corrupt_new_match_snapshot;
             UPDATE matches SET jc_changes='{}';",
        )
        .expect("repair malformed snapshot");
    let recovered = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("retry after snapshot repair");
    assert_eq!(recovered.match_id, match_id);
    assert_participant_rewards_paid_once(&fixture, match_id);
}

#[test]
fn snapshot_blood_pact_read_failure_does_not_consume_bonus_claim() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    Connection::open(fixture.database.path())
        .expect("hide Blood Pact durable-event table")
        .execute(
            "ALTER TABLE hostile_loss_events RENAME TO hostile_loss_events_unavailable",
            [],
        )
        .expect("hide Blood Pact durable-event table");

    let error = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect_err("Blood Pact snapshot failure stops before bonuses");
    assert!(error.contains("hostile_loss_events"));
    let match_id = recorded_match_id_for_pending(&fixture, pending.pending_match_id);
    assert_bonus_phase_not_started(&fixture, match_id);

    Connection::open(fixture.database.path())
        .expect("restore Blood Pact durable-event table")
        .execute(
            "ALTER TABLE hostile_loss_events_unavailable RENAME TO hostile_loss_events",
            [],
        )
        .expect("restore Blood Pact durable-event table");
    let recovered = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("retry after Blood Pact snapshot repair");
    assert_eq!(recovered.match_id, match_id);
    assert_participant_rewards_paid_once(&fixture, match_id);
}

fn assert_settled_bet_summary_survives_snapshot_write_failure(with_protection: bool) {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let winning_bettor = 82_301_i64;
    let losing_bettor = 82_302_i64;
    let skimmer = 82_303_i64;
    let players = PlayerRepository::new(fixture.database.path());
    for discord_id in [winning_bettor, losing_bettor, skimmer] {
        let mut player = NewPlayer::new(discord_id, format!("bettor-{discord_id}"), Some(GUILD));
        player.initial_mmr = Some(4_000);
        player.glicko_rating = Some(1_500.0);
        player.glicko_rd = Some(200.0);
        player.glicko_volatility = Some(0.06);
        player.os_mu = Some(CamaOpenSkillSystem::DEFAULT_MU);
        player.os_sigma = Some(CamaOpenSkillSystem::DEFAULT_SIGMA);
        players.add(&player).expect("insert betting spectator");
    }
    let connection = Connection::open(fixture.database.path()).expect("seed betting fixture");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=50
             WHERE guild_id=?1 AND discord_id IN (?2,?3)",
            params![GUILD, winning_bettor, losing_bettor],
        )
        .expect("fund bettors");
    if with_protection {
        connection
            .execute(
                "INSERT INTO manashop_buffs(
                     discord_id,guild_id,buff_type,target_id,
                     granted_at,expires_at,triggered,data
                 ) VALUES(?1,?2,'blood_pact',?3,0,?4,0,?5)",
                params![
                    skimmer,
                    GUILD,
                    winning_bettor,
                    i64::MAX,
                    r#"{"skim_rate":0.25,"cap":100,"skimmed_total":0}"#
                ],
            )
            .expect("grant betting Blood Pact");
    }
    connection
        .execute_batch(
            "CREATE TRIGGER fail_aggregate_match_snapshot
             BEFORE UPDATE OF jc_changes ON matches
             WHEN NEW.jc_changes LIKE '%\"payout\"%'
             BEGIN
               SELECT RAISE(ABORT,'injected aggregate snapshot failure');
             END;",
        )
        .expect("install aggregate snapshot failure");
    drop(connection);

    let betting = BettingServiceRepository::new(fixture.database.path());
    for (discord_id, team) in [
        (winning_bettor, BettingTeam::Radiant),
        (losing_bettor, BettingTeam::Dire),
    ] {
        betting
            .place_bet_atomic(PlaceBetRequest {
                guild_id: Some(GUILD),
                pending_match_id: pending.pending_match_id,
                discord_id,
                team,
                amount: 5,
                bet_time: pending.state.shuffle_timestamp.unwrap_or_default(),
                leverage: 1,
                max_debt: 500,
                is_blind: false,
                odds_at_placement: None,
            })
            .expect("place spectator bet");
    }

    let error = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect_err("aggregate snapshot write fails after bet settlement");
    assert!(error.contains("injected aggregate snapshot failure"));
    let match_id = recorded_match_id_for_pending(&fixture, pending.pending_match_id);
    let repository = MatchRepository::new(fixture.database.path());
    let durable = repository
        .get_match(match_id, Some(GUILD))
        .expect("read durable bet-only snapshot")
        .expect("recorded match exists");
    assert_eq!(durable.jc_changes[&losing_bettor]["bet"], -5);
    assert_eq!(
        durable.jc_changes[&winning_bettor]["bet"],
        if with_protection { 4 } else { 5 }
    );
    let connection = Connection::open(fixture.database.path()).expect("read post-failure balances");
    let balances_before_retry = connection
        .prepare(
            "SELECT discord_id,jopacoin_balance FROM players
             WHERE guild_id=?1 ORDER BY discord_id",
        )
        .expect("prepare post-failure balances")
        .query_map([GUILD], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("read post-failure balances")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect post-failure balances");
    drop(connection);

    Connection::open(fixture.database.path())
        .expect("remove aggregate snapshot failure")
        .execute("DROP TRIGGER fail_aggregate_match_snapshot", [])
        .expect("remove aggregate snapshot failure");
    let retry = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("retry completes aggregate snapshot");
    assert_eq!(retry.match_id, match_id);
    let recovered = repository
        .get_match(match_id, Some(GUILD))
        .expect("read recovered aggregate snapshot")
        .expect("recorded match exists");
    assert_eq!(recovered.jc_changes[&losing_bettor]["bet"], -5);
    assert_eq!(
        recovered.jc_changes[&winning_bettor]["bet"],
        if with_protection { 4 } else { 5 }
    );
    if with_protection {
        assert_eq!(
            recovered.jc_changes[&winning_bettor]["bet_blood_pact_skim"],
            1
        );
    }
    let connection = Connection::open(fixture.database.path()).expect("read retry balances");
    let balances_after_retry = connection
        .prepare(
            "SELECT discord_id,jopacoin_balance FROM players
             WHERE guild_id=?1 ORDER BY discord_id",
        )
        .expect("prepare retry balances")
        .query_map([GUILD], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("read retry balances")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect retry balances");
    assert_eq!(balances_after_retry, balances_before_retry);
}

#[test]
fn settled_bet_summary_survives_snapshot_write_failure_without_protection() {
    assert_settled_bet_summary_survives_snapshot_write_failure(false);
}

#[test]
fn settled_bet_summary_survives_snapshot_write_failure_with_protection() {
    assert_settled_bet_summary_survives_snapshot_write_failure(true);
}

#[test]
fn production_reward_saga_recovers_partial_payout_without_compensation_or_double_pay() {
    let fixture = MatchRuntimeFixture::new();
    let mut pending = fixture.pending(unix_seconds() + 120);
    let excluded_id = 81_120_i64;
    pending.state.excluded_player_ids = vec![excluded_id];
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .update_pending_match(GUILD, pending.pending_match_id, &pending.state)
            .expect("persist excluded-player reward dependency")
    );
    let initial_balance = Connection::open(fixture.database.path())
        .expect("inspect initial reward balance")
        .query_row(
            "SELECT COALESCE(SUM(jopacoin_balance),0) FROM players WHERE guild_id=?1",
            [GUILD],
            |row| row.get::<_, i64>(0),
        )
        .expect("read initial reward balance");

    // Participant and win income is credited before the missing exclusion
    // recipient makes the later phase fail. Those exact-once credits must stay
    // durable so READY/retry can resume at the failed phase.
    let first_error = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect_err("missing excluded player pauses reward saga");
    assert!(first_error.contains("match bonus payout failed"));
    let partial: (i64, i64) = Connection::open(fixture.database.path())
        .expect("inspect partial payout")
        .query_row(
            "SELECT
                 (SELECT COALESCE(SUM(jopacoin_balance),0) FROM players WHERE guild_id=?1),
                 (SELECT COUNT(*) FROM economy_ledger_entries
                  WHERE guild_id=?1 AND source IN ('match_participation','match_win_bonus'))",
            [GUILD],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read partial payout");
    assert_eq!(partial, (initial_balance + 110, 10));

    let mut excluded = NewPlayer::new(excluded_id, "excluded-reward", Some(GUILD));
    excluded.initial_mmr = Some(4_000);
    PlayerRepository::new(fixture.database.path())
        .add(&excluded)
        .expect("repair excluded-player dependency");
    let before_resume_balance = Connection::open(fixture.database.path())
        .expect("inspect repaired reward balance")
        .query_row(
            "SELECT COALESCE(SUM(jopacoin_balance),0) FROM players WHERE guild_id=?1",
            [GUILD],
            |row| row.get::<_, i64>(0),
        )
        .expect("read repaired reward balance");
    let recovered = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("resume partial reward saga");
    let after_recovery: (i64, i64, i64, i64) = Connection::open(fixture.database.path())
        .expect("inspect recovered payout")
        .query_row(
            "SELECT
                 (SELECT COALESCE(SUM(jopacoin_balance),0) FROM players WHERE guild_id=?1),
                 (SELECT COUNT(*) FROM economy_ledger_entries WHERE guild_id=?1),
                 (SELECT COUNT(*) FROM matches WHERE guild_id=?1),
                 (SELECT COUNT(*) FROM player_pairings WHERE guild_id=?1)",
            [GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read recovered payout");
    assert_eq!(after_recovery.0, before_resume_balance + 1);
    assert_eq!((after_recovery.2, after_recovery.3), (1, 45));

    let replay = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("replay fully recovered reward saga");
    assert_eq!(replay.match_id, recovered.match_id);
    let after_replay: (i64, i64, i64, i64) = Connection::open(fixture.database.path())
        .expect("inspect reward replay")
        .query_row(
            "SELECT
                 (SELECT COALESCE(SUM(jopacoin_balance),0) FROM players WHERE guild_id=?1),
                 (SELECT COUNT(*) FROM economy_ledger_entries WHERE guild_id=?1),
                 (SELECT COUNT(*) FROM matches WHERE guild_id=?1),
                 (SELECT COUNT(*) FROM player_pairings WHERE guild_id=?1)",
            [GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read reward replay");
    assert_eq!(after_replay, after_recovery);
}

#[test]
fn bonus_failure_recovers_claim_and_pays_every_player_exactly_once() {
    let fixture = MatchRuntimeFixture::new();
    let mut pending = fixture.pending(unix_seconds() + 120);
    let missing_recipient = 81_121_i64;
    pending.state.excluded_player_ids = vec![missing_recipient];
    PendingMatchRepository::new(fixture.database.path())
        .update_pending_match(GUILD, pending.pending_match_id, &pending.state)
        .expect("persist missing reward dependency");
    let participants_before = Connection::open(fixture.database.path())
        .expect("read participant baseline")
        .query_row(
            "SELECT COALESCE(SUM(jopacoin_balance),0) FROM players
             WHERE guild_id=?1 AND discord_id BETWEEN 81100 AND 81109",
            [GUILD],
            |row| row.get::<_, i64>(0),
        )
        .expect("participant baseline");

    fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect_err("missing exclusion recipient pauses claimed bonus phase");
    let match_id = recorded_match_id_for_pending(&fixture, pending.pending_match_id);
    let failed: (i64, i64, i64) = Connection::open(fixture.database.path())
        .expect("inspect released bonus claim")
        .query_row(
            "SELECT COALESCE(bonuses_paid,0),
                    (SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND related_id=?2
                       AND source IN ('match_participation','match_win_bonus')),
                    (SELECT COALESCE(SUM(jopacoin_balance),0) FROM players
                     WHERE guild_id=?1 AND discord_id BETWEEN 81100 AND 81109)
             FROM matches WHERE guild_id=?1 AND match_id=?2",
            params![GUILD, match_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read released bonus claim");
    assert_eq!(failed, (0, 10, participants_before + 110));

    let mut recipient = NewPlayer::new(
        missing_recipient,
        "repaired-exclusion-recipient",
        Some(GUILD),
    );
    recipient.initial_mmr = Some(4_000);
    PlayerRepository::new(fixture.database.path())
        .add(&recipient)
        .expect("repair exclusion reward dependency");
    let recovered = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("resume released bonus claim");
    assert_eq!(recovered.match_id, match_id);
    let completed: (i64, i64, i64, i64) = Connection::open(fixture.database.path())
        .expect("inspect completed bonus claim")
        .query_row(
            "SELECT COALESCE(bonuses_paid,0),
                    (SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND related_id=?2
                       AND source IN ('match_participation','match_win_bonus')),
                    (SELECT COALESCE(SUM(jopacoin_balance),0) FROM players
                     WHERE guild_id=?1 AND discord_id BETWEEN 81100 AND 81109),
                    (SELECT COUNT(*) FROM matches WHERE guild_id=?1)
             FROM matches WHERE guild_id=?1 AND match_id=?2",
            params![GUILD, match_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read completed bonus claim");
    assert_eq!(completed, (1, 10, participants_before + 110, 1));
}

#[test]
fn bonus_failure_compensates_partial_participation_and_releases_claim() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let participant_ids = pending
        .state
        .radiant_team_ids
        .iter()
        .chain(&pending.state.dire_team_ids)
        .copied()
        .collect::<Vec<_>>();
    let participant_balances_before = participant_ids
        .iter()
        .map(|discord_id| {
            (
                *discord_id,
                Connection::open(fixture.database.path())
                    .expect("open participant baseline")
                    .query_row(
                        "SELECT jopacoin_balance FROM players
                         WHERE guild_id=?1 AND discord_id=?2",
                        params![GUILD, discord_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .expect("read participant baseline"),
            )
        })
        .collect::<BTreeMap<_, _>>();

    // Settle two real bets before the reward failure.  The retry must recover
    // the already-settled bet rows without paying either bettor a second time.
    let winning_bettor = 82_401_i64;
    let losing_bettor = 82_402_i64;
    let players = PlayerRepository::new(fixture.database.path());
    for discord_id in [winning_bettor, losing_bettor] {
        let mut player = NewPlayer::new(discord_id, format!("bettor-{discord_id}"), Some(GUILD));
        player.initial_mmr = Some(4_000);
        player.glicko_rating = Some(1_500.0);
        player.glicko_rd = Some(200.0);
        player.glicko_volatility = Some(0.06);
        player.os_mu = Some(CamaOpenSkillSystem::DEFAULT_MU);
        player.os_sigma = Some(CamaOpenSkillSystem::DEFAULT_SIGMA);
        players.add(&player).expect("insert bettor");
    }
    let betting = BettingServiceRepository::new(fixture.database.path());
    for (discord_id, team) in [
        (winning_bettor, BettingTeam::Radiant),
        (losing_bettor, BettingTeam::Dire),
    ] {
        Connection::open(fixture.database.path())
            .expect("open bettor balance")
            .execute(
                "UPDATE players SET jopacoin_balance=50
                 WHERE guild_id=?1 AND discord_id=?2",
                params![GUILD, discord_id],
            )
            .expect("seed bettor balance");
        betting
            .place_bet_atomic(PlaceBetRequest {
                guild_id: Some(GUILD),
                pending_match_id: pending.pending_match_id,
                discord_id,
                team,
                amount: 5,
                bet_time: pending.state.shuffle_timestamp.unwrap_or_default(),
                leverage: 1,
                max_debt: 500,
                is_blind: false,
                odds_at_placement: None,
            })
            .expect("place bettor wager");
    }

    let trigger_connection =
        Connection::open(fixture.database.path()).expect("open reward failure fixture");
    trigger_connection
        .execute_batch(
            "CREATE TRIGGER fail_match_win_bonus_after_participation
             BEFORE UPDATE OF jopacoin_balance ON players
             WHEN NEW.guild_id = 82001
               AND EXISTS (
                   SELECT 1 FROM economy_ledger_context
                   WHERE id=1 AND source='match_win_bonus'
               )
             BEGIN
               SELECT RAISE(ABORT,'injected match win bonus failure');
             END;",
        )
        .expect("install win-bonus failure");

    let first_error = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect_err("partial participation failure must be surfaced");
    assert!(first_error.contains("match bonus payout failed"));
    let match_id = recorded_match_id_for_pending(&fixture, pending.pending_match_id);

    let failed_state: (i64, i64, i64, i64) = Connection::open(fixture.database.path())
        .expect("inspect compensated reward failure")
        .query_row(
            "SELECT COALESCE(bonuses_paid,0),
                    (SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND related_id=?2
                       AND source='match_participation'),
                    (SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND related_id=?2
                       AND source='match_bonus_rollback'),
                    (SELECT COUNT(*) FROM matches WHERE guild_id=?1)
             FROM matches WHERE guild_id=?1 AND match_id=?2",
            params![GUILD, match_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read compensated reward failure");
    assert_eq!(failed_state, (0, 5, 5, 1));
    for (discord_id, balance) in &participant_balances_before {
        assert_eq!(
            Connection::open(fixture.database.path())
                .expect("open compensated participant")
                .query_row(
                    "SELECT jopacoin_balance FROM players
                     WHERE guild_id=?1 AND discord_id=?2",
                    params![GUILD, discord_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("read compensated participant"),
            *balance,
            "partial participation must be compensated before claim release"
        );
    }
    let bettors_after_failure = Connection::open(fixture.database.path())
        .expect("inspect settled bettor balances")
        .prepare(
            "SELECT discord_id,jopacoin_balance FROM players
             WHERE guild_id=?1 AND discord_id IN (?2,?3) ORDER BY discord_id",
        )
        .expect("prepare settled bettor balances")
        .query_map(params![GUILD, winning_bettor, losing_bettor], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("read settled bettor balances")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect settled bettor balances");

    trigger_connection
        .execute("DROP TRIGGER fail_match_win_bonus_after_participation", [])
        .expect("remove win-bonus failure");
    let recovered = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("retry after compensated reward failure");
    assert_eq!(recovered.match_id, match_id);

    for (index, discord_id) in participant_ids.iter().enumerate() {
        let expected = participant_balances_before[discord_id] + if index < 5 { 20 } else { 2 };
        let actual = Connection::open(fixture.database.path())
            .expect("open recovered participant")
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE guild_id=?1 AND discord_id=?2",
                params![GUILD, discord_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("read recovered participant");
        assert_eq!(
            actual, expected,
            "retry must pay each participant exactly once"
        );
    }
    let recovered_state: (i64, i64, i64, i64, i64, i64) = Connection::open(fixture.database.path())
        .expect("inspect recovered reward")
        .query_row(
            "SELECT COALESCE(bonuses_paid,0),
                    (SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND related_id=?2
                       AND source IN ('match_participation','match_win_bonus')),
                    (SELECT COALESCE(SUM(delta),0) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND related_id=?2
                       AND source IN ('match_participation','match_win_bonus')),
                    (SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND related_id=?2
                       AND source='match_bonus_rollback'),
                    (SELECT COUNT(*) FROM matches WHERE guild_id=?1),
                    (SELECT COUNT(*) FROM bets WHERE guild_id=?1 AND match_id=?2)
             FROM matches WHERE guild_id=?1 AND match_id=?2",
            params![GUILD, match_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("read recovered reward");
    assert_eq!(recovered_state, (1, 15, 120, 0, 1, 2));

    let bettors_after_retry = Connection::open(fixture.database.path())
        .expect("inspect retry bettor balances")
        .prepare(
            "SELECT discord_id,jopacoin_balance FROM players
             WHERE guild_id=?1 AND discord_id IN (?2,?3) ORDER BY discord_id",
        )
        .expect("prepare retry bettor balances")
        .query_map(params![GUILD, winning_bettor, losing_bettor], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("read retry bettor balances")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect retry bettor balances");
    assert_eq!(bettors_after_retry, bettors_after_failure);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extend_betting_updates_the_live_lock_until() {
    let fixture = MatchRuntimeFixture::new();
    let original_lock = unix_seconds() + 600;
    let pending = fixture.pending(original_lock);
    let outcome = fixture
        .provider
        .handler
        .extend_betting(AdminExtendBettingRequest {
            guild_id: GUILD,
            actor_id: 99,
            minutes: 10,
            pending_match_id: pending.pending_match_id,
        })
        .await
        .expect("extend live betting window");
    assert_eq!(outcome.old_bet_lock_until, original_lock);
    assert_eq!(outcome.new_bet_lock_until, original_lock + 600);
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extended_betting_is_persisted_to_the_pending_match_row() {
    let fixture = MatchRuntimeFixture::new();
    let original_lock = unix_seconds() + 600;
    let pending = fixture.pending(original_lock);
    fixture
        .provider
        .handler
        .extend_betting(AdminExtendBettingRequest {
            guild_id: GUILD,
            actor_id: 99,
            minutes: 30,
            pending_match_id: pending.pending_match_id,
        })
        .await
        .expect("persist betting extension");
    assert_eq!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read persisted betting extension")
            .expect("pending match remains")
            .state
            .bet_lock_until,
        Some(original_lock + 1_800)
    );
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extended_betting_survives_a_fresh_repository_after_restart() {
    let fixture = MatchRuntimeFixture::new();
    let original_lock = unix_seconds() + 600;
    let pending = fixture.pending(original_lock);
    fixture
        .provider
        .handler
        .extend_betting(AdminExtendBettingRequest {
            guild_id: GUILD,
            actor_id: 99,
            minutes: 60,
            pending_match_id: pending.pending_match_id,
        })
        .await
        .expect("extend betting before restart");
    let restarted_repository = PendingMatchRepository::new(fixture.database.path());
    assert_eq!(
        restarted_repository
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read extension after restart")
            .expect("pending match survives restart")
            .state
            .bet_lock_until,
        Some(original_lock + 3_600)
    );
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extending_an_expired_window_reopens_atomic_bet_placement() {
    let fixture = MatchRuntimeFixture::new();
    let now = unix_seconds();
    let pending = fixture.pending(now - 60);
    let bettor = pending.state.radiant_team_ids[0];
    let bets = BettingServiceRepository::new(fixture.database.path());
    let request = PlaceBetRequest {
        guild_id: Some(GUILD),
        pending_match_id: pending.pending_match_id,
        discord_id: bettor,
        team: BettingTeam::Radiant,
        amount: 1,
        bet_time: now,
        leverage: 1,
        max_debt: 1_000,
        is_blind: false,
        odds_at_placement: None,
    };
    assert!(matches!(
        bets.place_bet_atomic(request),
        Err(BettingServiceRepositoryError::BettingClosed)
    ));
    fixture
        .provider
        .handler
        .extend_betting(AdminExtendBettingRequest {
            guild_id: GUILD,
            actor_id: 99,
            minutes: 10,
            pending_match_id: pending.pending_match_id,
        })
        .await
        .expect("reopen expired betting window");
    bets.place_bet_atomic(request)
        .expect("bet succeeds after extension");
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_betting_extension_uses_current_time_as_its_base() {
    let fixture = MatchRuntimeFixture::new();
    let expired_lock = unix_seconds() - 300;
    let pending = fixture.pending(expired_lock);
    let before = unix_seconds();
    let outcome = fixture
        .provider
        .handler
        .extend_betting(AdminExtendBettingRequest {
            guild_id: GUILD,
            actor_id: 99,
            minutes: 5,
            pending_match_id: pending.pending_match_id,
        })
        .await
        .expect("extend expired betting window");
    let after = unix_seconds();
    assert_eq!(outcome.old_bet_lock_until, expired_lock);
    assert!(outcome.new_bet_lock_until >= before + 300);
    assert!(outcome.new_bet_lock_until <= after + 300);
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_betting_extensions_accumulate_from_the_latest_lock() {
    let fixture = MatchRuntimeFixture::new();
    let original_lock = unix_seconds() + 600;
    let pending = fixture.pending(original_lock);
    let first = fixture
        .provider
        .handler
        .extend_betting(AdminExtendBettingRequest {
            guild_id: GUILD,
            actor_id: 99,
            minutes: 5,
            pending_match_id: pending.pending_match_id,
        })
        .await
        .expect("first betting extension");
    let second = fixture
        .provider
        .handler
        .extend_betting(AdminExtendBettingRequest {
            guild_id: GUILD,
            actor_id: 99,
            minutes: 10,
            pending_match_id: pending.pending_match_id,
        })
        .await
        .expect("second betting extension");
    assert_eq!(first.new_bet_lock_until, original_lock + 300);
    assert_eq!(second.old_bet_lock_until, first.new_bet_lock_until);
    assert_eq!(second.new_bet_lock_until, original_lock + 900);
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_extension_mutates_only_selected_pending_and_replaces_its_reminders() {
    let fixture = MatchRuntimeFixture::new();
    let repository = PendingMatchRepository::new(fixture.database.path());
    let original_lock = unix_seconds() + 120;
    let first = repository
        .create_pending_match(
            GUILD,
            &PendingMatchState {
                bet_lock_until: Some(original_lock),
                lock_time: Some(original_lock),
                lobby_kind: Some("open".to_owned()),
                ..PendingMatchState::default()
            },
        )
        .expect("first pending match");
    let selected = repository
        .create_pending_match(
            GUILD,
            &PendingMatchState {
                bet_lock_until: Some(original_lock),
                lock_time: Some(original_lock),
                lobby_kind: Some("lowskill".to_owned()),
                ..PendingMatchState::default()
            },
        )
        .expect("selected pending match");

    let outcome = fixture
        .provider
        .handler
        .extend_betting(AdminExtendBettingRequest {
            guild_id: GUILD,
            actor_id: 99,
            minutes: 5,
            pending_match_id: selected.pending_match_id,
        })
        .await
        .expect("extend selected betting window");
    assert_eq!(outcome.pending_match_id, selected.pending_match_id);
    assert_eq!(outcome.old_bet_lock_until, original_lock);
    assert_eq!(outcome.new_bet_lock_until, original_lock + 300);
    assert_eq!(outcome.lobby_label, "🧀 Whine & Cheese");
    assert_eq!(outcome.refreshed_routes, 0);
    assert!(outcome.refresh_failures.is_empty());
    assert_eq!(
        repository
            .pending_match(GUILD, first.pending_match_id)
            .expect("read first pending")
            .expect("first pending exists")
            .state
            .bet_lock_until,
        Some(original_lock)
    );
    assert_eq!(
        repository
            .pending_match(GUILD, selected.pending_match_id)
            .expect("read selected pending")
            .expect("selected pending exists")
            .state
            .bet_lock_until,
        Some(original_lock + 300)
    );
    let mut tasks = fixture
        .provider
        .handler
        .betting_tasks
        .lock()
        .expect("extension task registry");
    assert!(!tasks.contains_key(&(GUILD, first.pending_match_id)));
    let selected_tasks = tasks
        .remove(&(GUILD, selected.pending_match_id))
        .expect("selected reminders scheduled");
    assert_eq!(selected_tasks.len(), 3);
    for task in selected_tasks {
        task.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_record_override_commits_and_clears_the_production_pending_match() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let responder = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_record(
            record_context(99_001, GUILD, "radiant", true),
            responder.clone(),
        )
        .await
        .expect("admin override records match");
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read admin-record pending")
            .is_none()
    );
    assert_eq!(
        Connection::open(fixture.database.path())
            .expect("inspect admin record")
            .query_row(
                "SELECT COUNT(*) FROM matches WHERE guild_id=?1",
                [GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("count admin-recorded matches"),
        1
    );
    assert!(
        responder
            .contents()
            .iter()
            .any(|content| content.contains("Match recorded"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn non_admin_record_requires_three_matching_participant_submissions() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let voters = &pending.state.radiant_team_ids[..3];
    let first = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_record(
            record_context(voters[0], GUILD, "radiant", false),
            first.clone(),
        )
        .await
        .expect("first participant vote");
    assert!(
        first
            .contents()
            .iter()
            .any(|content| content.contains("1/3"))
    );
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read pending after first vote")
            .is_some()
    );

    let second = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_record(
            record_context(voters[1], GUILD, "radiant", false),
            second.clone(),
        )
        .await
        .expect("second participant vote");
    assert!(
        second
            .contents()
            .iter()
            .any(|content| content.contains("2/3"))
    );

    let third = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_record(
            record_context(voters[2], GUILD, "radiant", false),
            third.clone(),
        )
        .await
        .expect("third participant vote records match");
    assert!(
        third
            .contents()
            .iter()
            .any(|content| content.contains("Match recorded"))
    );
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read pending after quorum")
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shuffle_and_record_pending_state_is_strictly_guild_scoped() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let wrong_guild = GUILD + 1;
    let rejected = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_record(
            record_context(
                pending.state.radiant_team_ids[0],
                wrong_guild,
                "radiant",
                true,
            ),
            rejected.clone(),
        )
        .await
        .expect("wrong guild receives normal rejection");
    assert!(
        rejected
            .contents()
            .iter()
            .any(|content| content.contains("No pending match"))
    );
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read original guild pending")
            .is_some()
    );

    let accepted = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_record(
            record_context(99_002, GUILD, "dire", true),
            accepted.clone(),
        )
        .await
        .expect("correct guild records match");
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read correctly scoped pending")
            .is_none()
    );
    assert_eq!(
        Connection::open(fixture.database.path())
            .expect("inspect guild-scoped record")
            .query_row(
                "SELECT COUNT(*) FROM matches WHERE guild_id=?1",
                [wrong_guild],
                |row| row.get::<_, i64>(0),
            )
            .expect("count wrong-guild matches"),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_match_service_repo_injected_shuffle_and_record() {
    let fixture = MatchRuntimeFixture::new();
    assert!(
        last_match_participant_ids(fixture.database.path(), GUILD)
            .expect("empty last-match lookup")
            .is_empty()
    );
    let player_ids = fixture.add_shuffle_pool(10, false);
    let prepared = fixture.prepare_shuffle(player_ids.clone(), "glicko", Vec::new());
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, prepared.pending.pending_match_id)
            .expect("load prepared match")
            .is_some()
    );

    let responder = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_record(
            record_context(player_ids[0], GUILD, "radiant", true),
            responder.clone(),
        )
        .await
        .expect("record prepared match through live handler");
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, prepared.pending.pending_match_id)
            .expect("load cleared prepared match")
            .is_none()
    );
    assert_eq!(
        last_match_participant_ids(fixture.database.path(), GUILD)
            .expect("load recorded participants"),
        player_ids.into_iter().collect()
    );
    assert!(
        responder
            .contents()
            .iter()
            .any(|content| content.contains("Match recorded"))
    );
}

#[test]
fn test_record_match_stores_predictions_and_history() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    let prepared = fixture.prepare_shuffle(player_ids.clone(), "glicko", Vec::new());
    let recorded = fixture
        .provider
        .handler
        .record_match_blocking(&prepared.pending, "radiant", None)
        .expect("record production match");
    let connection = Connection::open(fixture.database.path()).expect("open recorded match");

    let prediction: (i64, Option<f64>) = connection
        .query_row(
            "SELECT match_id,expected_radiant_win_prob
             FROM match_predictions WHERE match_id=?1",
            [recorded.match_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read recorded match prediction");
    assert_eq!(prediction.0, recorded.match_id);
    assert_eq!(prediction.1, Some(0.5));

    let history = connection
        .prepare(
            "SELECT rating_before,rd_before,os_mu_before,os_mu_after,
                    os_sigma_before,os_sigma_after,expected_team_win_prob,
                    team_number,won
             FROM rating_history
             WHERE guild_id=?1 AND match_id=?2
             ORDER BY discord_id",
        )
        .expect("prepare recorded rating history")
        .query_map(params![GUILD, recorded.match_id], |row| {
            Ok((
                row.get::<_, Option<f64>>(0)?,
                row.get::<_, Option<f64>>(1)?,
                row.get::<_, Option<f64>>(2)?,
                row.get::<_, Option<f64>>(3)?,
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, Option<f64>>(5)?,
                row.get::<_, Option<f64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<bool>>(8)?,
            ))
        })
        .expect("read recorded rating history")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect recorded rating history");
    assert_eq!(history.len(), 10);
    for (
        rating_before,
        rd_before,
        os_mu_before,
        os_mu_after,
        os_sigma_before,
        os_sigma_after,
        expected_team_win_prob,
        team_number,
        won,
    ) in history
    {
        assert!(rating_before.is_some());
        assert!(rd_before.is_some());
        assert!(os_mu_before.is_some());
        assert!(os_mu_after.is_some());
        assert!(os_sigma_before.is_some());
        assert!(os_sigma_after.is_some());
        assert_eq!(expected_team_win_prob, Some(0.5));
        assert!(matches!(team_number, Some(1 | 2)));
        assert!(matches!(won, Some(true | false)));
    }

    let target: (Option<i64>, Option<f64>) = connection
        .query_row(
            "SELECT streak_length,streak_multiplier
             FROM rating_history
             WHERE guild_id=?1 AND match_id=?2 AND discord_id=?3",
            params![GUILD, recorded.match_id, player_ids[0]],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read target player rating history");
    assert_eq!(target.0, Some(1));
    assert_eq!(target.1, Some(1.0));
}

#[test]
fn test_shuffle_and_record_connection_budgets() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    let snapshot = fixture
        .provider
        .handler
        .players
        .get_shuffle_inputs(&player_ids, Some(GUILD))
        .expect("load one batched shuffle snapshot");
    assert_eq!(snapshot.players.len(), 10);
    assert_eq!(snapshot.exclusion_counts.len(), 10);

    let prepared = fixture.prepare_shuffle(player_ids, "glicko", Vec::new());
    let recorded = fixture
        .provider
        .handler
        .record_match_blocking(&prepared.pending, "radiant", None)
        .expect("record through one atomic core transaction");
    assert!(recorded.match_id > 0);
    let connection = Connection::open(fixture.database.path()).expect("inspect batched hot path");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM match_participants WHERE match_id=?1",
                [recorded.match_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("count atomically inserted participants"),
        10
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM rating_history WHERE match_id=?1",
                [recorded.match_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("count batched rating history"),
        10
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_shuffle_and_abort_leave_exclusion_factors_unchanged() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(12, false);
    let before = fixture
        .provider
        .handler
        .players
        .get_exclusion_counts(&player_ids, Some(GUILD))
        .expect("read pre-shuffle exclusion factors");
    let prepared = fixture.prepare_shuffle(player_ids.clone(), "glicko", Vec::new());
    assert!(prepared.pending.state.exclusion_updates_deferred);
    assert_eq!(
        fixture
            .provider
            .handler
            .players
            .get_exclusion_counts(&player_ids, Some(GUILD))
            .expect("read persisted-shuffle exclusion factors"),
        before
    );
    fixture
        .provider
        .handler
        .finalize_abort_owned(&prepared.pending)
        .await
        .expect("abort prepared match");
    assert_eq!(
        fixture
            .provider
            .handler
            .players
            .get_exclusion_counts(&player_ids, Some(GUILD))
            .expect("read aborted exclusion factors"),
        before
    );
}

#[test]
fn test_record_applies_deferred_exclusion_factor_updates() {
    let fixture = MatchRuntimeFixture::new();
    let mut player_ids = fixture.add_shuffle_pool(13, false);
    let conditional_id = player_ids.pop().expect("conditional fixture player");
    let before = fixture
        .provider
        .handler
        .players
        .get_exclusion_counts(
            &player_ids
                .iter()
                .copied()
                .chain([conditional_id])
                .collect::<Vec<_>>(),
            Some(GUILD),
        )
        .expect("read pre-record exclusion factors");
    let prepared = fixture.prepare_shuffle(player_ids, "glicko", vec![conditional_id]);
    assert_eq!(
        prepared.pending.state.half_exclusion_increment_ids,
        [conditional_id]
    );
    fixture
        .provider
        .handler
        .record_match_blocking(&prepared.pending, "radiant", None)
        .expect("record deferred exclusion updates");
    let after = fixture
        .provider
        .handler
        .players
        .get_exclusion_counts(
            &prepared
                .pending
                .state
                .full_lobby_player_ids()
                .into_iter()
                .collect::<Vec<_>>(),
            Some(GUILD),
        )
        .expect("read applied exclusion factors");
    for player_id in prepared.pending.state.participant_ids() {
        assert_eq!(
            after.get(&player_id).copied().expect("participant after"),
            (before.get(&player_id).copied().expect("participant before") - 1).max(0)
        );
    }
    for player_id in &prepared.pending.state.full_exclusion_increment_ids {
        assert_eq!(
            after.get(player_id).copied().expect("excluded after"),
            before.get(player_id).copied().expect("excluded before") + 6
        );
    }
    assert_eq!(
        after
            .get(&conditional_id)
            .copied()
            .expect("conditional after"),
        before
            .get(&conditional_id)
            .copied()
            .expect("conditional before")
            + 1
    );
}

#[test]
fn test_openskill_falls_back_to_glicko_when_player_missing_os_mu() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    let prepared = fixture.prepare_shuffle(player_ids, "openskill", Vec::new());
    assert_eq!(prepared.pending.state.balancing_rating_system, "glicko");
    assert!(!prepared.pending.state.is_openskill_shuffle);
}

#[test]
fn test_openskill_used_when_all_players_have_os_mu() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, true);
    let prepared = fixture.prepare_shuffle(player_ids, "openskill", Vec::new());
    assert_eq!(prepared.pending.state.balancing_rating_system, "openskill");
    assert!(prepared.pending.state.is_openskill_shuffle);
}

#[test]
fn soft_avoid_is_not_decremented_by_persisting_a_shuffle_alone() {
    let fixture = MatchRuntimeFixture::new();
    let mut pending = fixture.pending(unix_seconds() + 120);
    let avoid_repository = SoftAvoidRepository::new(fixture.database.path());
    let avoid = avoid_repository
        .create_or_reactivate_avoid(
            Some(GUILD),
            pending.state.radiant_team_ids[0],
            pending.state.dire_team_ids[0],
            10,
        )
        .expect("create shuffle avoid");
    pending.state.effective_avoid_ids = vec![avoid.id];
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .update_pending_match(GUILD, pending.pending_match_id, &pending.state)
            .expect("persist effective shuffle avoid")
    );
    assert_eq!(
        avoid_repository
            .get_active_avoids_for_players(
                Some(GUILD),
                &[
                    pending.state.radiant_team_ids[0],
                    pending.state.dire_team_ids[0]
                ],
            )
            .expect("read shuffle-only avoid")[0]
            .games_remaining,
        10
    );
}

#[test]
fn soft_avoid_decrements_once_only_after_recording_opposite_teams() {
    let fixture = MatchRuntimeFixture::new();
    let mut pending = fixture.pending(unix_seconds() + 120);
    let avoid_repository = SoftAvoidRepository::new(fixture.database.path());
    let avoid = avoid_repository
        .create_or_reactivate_avoid(
            Some(GUILD),
            pending.state.radiant_team_ids[0],
            pending.state.dire_team_ids[0],
            10,
        )
        .expect("create record avoid");
    pending.state.effective_avoid_ids = vec![avoid.id];
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .update_pending_match(GUILD, pending.pending_match_id, &pending.state)
            .expect("persist record avoid")
    );
    fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("record avoided opponents");
    let remaining = || {
        avoid_repository
            .get_active_avoids_for_players(
                Some(GUILD),
                &[
                    pending.state.radiant_team_ids[0],
                    pending.state.dire_team_ids[0],
                ],
            )
            .expect("read recorded avoid")[0]
            .games_remaining
    };
    assert_eq!(remaining(), 9);
    fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("retry recorded avoided opponents");
    assert_eq!(remaining(), 9);
}

#[test]
fn production_goodness_score_includes_configured_soft_avoid_penalty() {
    let fixture = MatchRuntimeFixture::new();
    let radiant = [1_i64, 2, 3, 4, 5].into_iter().collect::<HashSet<_>>();
    let dire = [6_i64, 7, 8, 9, 10].into_iter().collect::<HashSet<_>>();
    let avoids = [SoftAvoid {
        avoider_discord_id: 1,
        avoided_discord_id: 2,
    }];
    let mut shuffler = BalancedShuffler::default();
    shuffler.soft_avoid_penalty = fixture.provider.handler.config.soft_avoid_penalty;
    assert_eq!(
        shuffler.calculate_soft_avoid_penalty(&radiant, &dire, Some(&avoids), None),
        shuffler.soft_avoid_penalty
    );
}

#[test]
fn test_goodness_includes_low_priority_selection_penalty_and_team_bonus() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    let baseline = fixture.prepare_shuffle(player_ids.clone(), "glicko", Vec::new());
    let low_priority_ids = player_ids[..2].to_vec();
    let connection =
        Connection::open(fixture.database.path()).expect("open low-priority goodness fixture");
    for player_id in &low_priority_ids {
        connection
            .execute(
                "INSERT INTO low_priority_state(
                     discord_id,guild_id,wins_required,wins_remaining,
                     start_pending_match_id,active,set_by
                 ) VALUES(?1,?2,3,3,NULL,1,901)",
                params![player_id, GUILD],
            )
            .expect("seed active low-priority player");
    }
    let penalized = fixture.prepare_shuffle(player_ids, "glicko", Vec::new());
    let low_priority = low_priority_ids.iter().copied().collect::<HashSet<_>>();
    let radiant = penalized
        .pending
        .state
        .radiant_team_ids
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let dire = penalized
        .pending
        .state
        .dire_team_ids
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    assert!(low_priority.is_subset(&radiant) || low_priority.is_subset(&dire));
    let baseline_score =
        extra_f64(&baseline.pending.state, "goodness_score").expect("baseline goodness");
    let penalized_score =
        extra_f64(&penalized.pending.state, "goodness_score").expect("penalized goodness");
    assert_eq!(penalized_score - baseline_score, 1_100.0);
}

#[test]
fn test_shuffle_display_uses_configured_flat_off_role_value_penalty() {
    let mut config = production_test_config();
    config.values.off_role_flat_value_penalty = 200.0;
    let fixture = MatchRuntimeFixture::new_with_config_and_discord(
        config,
        Arc::new(PublicationDiscord::default()),
    );
    let player_specs = [
        (5_000_i64, 3_000_i64, "1"),
        (5_001, 2_800, "2"),
        (5_002, 2_600, "3"),
        (5_003, 2_400, "4"),
        (5_004, 2_200, "5"),
        (5_005, 2_000, "2"),
        (5_006, 2_700, "2"),
        (5_007, 2_500, "3"),
        (5_008, 2_300, "4"),
        (5_009, 2_100, "5"),
    ];
    let player_ids = fixture.add_shuffle_pool_from(5_000, player_specs.len(), false);
    let repository = PlayerRepository::new(fixture.database.path());
    for (player_id, rating, preferred_role) in player_specs {
        repository
            .update_roles(player_id, Some(GUILD), &[preferred_role.to_owned()])
            .expect("set configured display role");
        repository
            .update_glicko_rating(player_id, Some(GUILD), rating as f64, 350.0, 0.06)
            .expect("set configured display rating");
    }
    let inputs = repository
        .get_shuffle_inputs(&player_ids, Some(GUILD))
        .expect("load configured display players");
    let players_by_id = player_ids
        .iter()
        .map(|player_id| {
            (
                *player_id,
                inputs
                    .players
                    .iter()
                    .find(|player| player.discord_id == Some(*player_id))
                    .cloned()
                    .expect("configured display player"),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let team = |ids: std::ops::Range<i64>, roles: [&str; 5]| {
        Team::new(
            ids.map(|player_id| players_by_id[&player_id].clone())
                .collect(),
            Some(roles.map(str::to_owned).to_vec()),
        )
        .expect("fixed display team")
    };
    let radiant = team(5_000..5_005, ["1", "2", "3", "4", "5"]);
    let dire = team(5_005..5_010, ["1", "2", "3", "4", "5"]);
    assert_eq!(
        fixture.provider.handler.config.off_role_flat_value_penalty,
        200.0
    );
    let configured = [radiant, dire]
        .into_iter()
        .map(|team| {
            team.get_team_value_with_off_role_value_penalty(
                true,
                false,
                false,
                fixture.provider.handler.config.off_role_flat_value_penalty,
            )
            .expect("calculate configured display value")
        })
        .collect::<Vec<_>>();
    // These fixture players have no recorded role history, so every one takes
    // the 0.95 role-performance floor. The drop from the pre-role-factor
    // values [13_000, 11_300] is not a uniform 5% because on-role players are
    // now scaled too, where the old off-role-only multiplier left them alone.
    assert_eq!(configured, [12_350.0, 10_820.0]);
}

#[test]
fn record_match_applies_one_exclusion_decay_per_inclusion_exactly_once() {
    let fixture = MatchRuntimeFixture::new();
    let mut pending = fixture.pending(unix_seconds() + 120);
    let participant = pending.state.radiant_team_ids[0];
    Connection::open(fixture.database.path())
        .expect("seed exclusion decay")
        .execute(
            "UPDATE players SET exclusion_count=9 WHERE guild_id=?1 AND discord_id=?2",
            params![GUILD, participant],
        )
        .expect("prime inclusion exclusion count");
    pending.state.exclusion_updates_deferred = true;
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .update_pending_match(GUILD, pending.pending_match_id, &pending.state)
            .expect("persist deferred exclusion decay")
    );
    let record = || {
        fixture
            .provider
            .handler
            .record_match_blocking(&pending, "radiant", None)
            .expect("record exclusion decay")
    };
    record();
    let exclusion_count = || {
        Connection::open(fixture.database.path())
            .expect("inspect exclusion decay")
            .query_row(
                "SELECT exclusion_count FROM players WHERE guild_id=?1 AND discord_id=?2",
                params![GUILD, participant],
                |row| row.get::<_, i64>(0),
            )
            .expect("read exclusion decay")
    };
    assert_eq!(exclusion_count(), 8);
    record();
    assert_eq!(exclusion_count(), 8);
}

#[test]
fn record_match_updates_every_rating_and_last_match_date_without_extra_decay() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("record rating update");
    let updated: (i64, i64, i64) = Connection::open(fixture.database.path())
        .expect("inspect rating update")
        .query_row(
            "SELECT
                 COUNT(*),
                 SUM(CASE WHEN glicko_rating != 1500.0 THEN 1 ELSE 0 END),
                 SUM(CASE WHEN last_match_date IS NOT NULL THEN 1 ELSE 0 END)
             FROM players
             WHERE guild_id=?1 AND discord_id BETWEEN 81100 AND 81109",
            [GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read rating update");
    assert_eq!(updated, (10, 10, 10));
    for discord_id in pending.state.participant_ids() {
        let (inputs, _) = fixture
            .provider
            .handler
            .players
            .get_match_rating_inputs_with_openskill_revision(&[discord_id], Some(GUILD))
            .expect("reload just-played rating");
        let input = inputs.get(&discord_id).expect("just-played rating exists");
        assert!(sql_f64(&input.glicko_rd).is_some_and(|rd| rd <= 200.0));
    }
}

#[test]
fn test_final_low_priority_win_boosts_both_rating_gains_before_release() {
    let fixture = MatchRuntimeFixture::new();
    let (pending, target, peer) = fixture.final_low_priority_pending();
    let recorded = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("record final low-priority win");

    let connection = Connection::open(fixture.database.path()).expect("inspect rating gains");
    let player = |discord_id| {
        connection
            .query_row(
                "SELECT glicko_rating,os_mu FROM players
                 WHERE guild_id=?1 AND discord_id=?2",
                params![GUILD, discord_id],
                |row| Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?)),
            )
            .expect("read boosted player")
    };
    let (target_rating, target_mu) = player(target);
    let (peer_rating, peer_mu) = player(peer);
    assert!((target_rating - 1_500.0 - (peer_rating - 1_500.0) * 1.10).abs() < 1e-9);
    assert!((target_mu - 25.0 - (peer_mu - 25.0) * 1.10).abs() < 1e-9);

    let multiplier = |discord_id| {
        connection
            .query_row(
                "SELECT low_priority_gain_multiplier FROM rating_history
                 WHERE guild_id=?1 AND match_id=?2 AND discord_id=?3",
                params![GUILD, recorded.match_id, discord_id],
                |row| row.get::<_, f64>(0),
            )
            .expect("read recording-time gain multiplier")
    };
    assert!((multiplier(target) - 1.10).abs() < 1e-12);
    assert!((multiplier(peer) - 1.0).abs() < 1e-12);
    assert_eq!(
        connection
            .query_row(
                "SELECT active,wins_remaining FROM low_priority_state
                 WHERE guild_id=?1 AND discord_id=?2",
                params![GUILD, target],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .expect("read released low-priority state"),
        (0, 0)
    );
}

#[test]
fn test_openskill_replay_retains_recorded_low_priority_gain() {
    let fixture = MatchRuntimeFixture::new();
    let (pending, target, peer) = fixture.final_low_priority_pending();
    let recorded = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect("record low-priority replay fixture");
    let corrections = MatchCorrectionRepository::new(fixture.database.path());
    corrections
        .mark_openskill_replay_pending(Some(GUILD), "low-priority parity replay")
        .expect("request durable replay");
    let summary = corrections
        .replay_openskill_atomic(Some(GUILD))
        .expect("replay recorded low-priority gain");
    assert!(summary.errors.is_empty());
    assert_eq!(summary.matches_processed, 1);

    let connection = Connection::open(fixture.database.path()).expect("inspect replay result");
    let replay_delta = |discord_id| {
        connection
            .query_row(
                "SELECT p.os_mu-rh.os_mu_before
                 FROM players p JOIN rating_history rh
                   ON rh.discord_id=p.discord_id AND rh.guild_id=p.guild_id
                 WHERE p.guild_id=?1 AND rh.match_id=?2 AND p.discord_id=?3",
                params![GUILD, recorded.match_id, discord_id],
                |row| row.get::<_, f64>(0),
            )
            .expect("read replay delta")
    };
    assert!((replay_delta(target) - replay_delta(peer) * 1.10).abs() < 1e-9);
    assert_eq!(
        connection
            .query_row(
                "SELECT low_priority_gain_multiplier FROM rating_history
                 WHERE guild_id=?1 AND match_id=?2 AND discord_id=?3",
                params![GUILD, recorded.match_id, target],
                |row| row.get::<_, f64>(0),
            )
            .expect("read persisted replay multiplier"),
        1.10
    );
}

#[test]
fn production_rating_decay_starts_only_beyond_the_seven_day_grace() {
    let fixture = MatchRuntimeFixture::new();
    let rating = &fixture.provider.handler.rating;
    assert_eq!(rating.apply_rd_decay(100.0, 7), 100.0);
    assert!(rating.apply_rd_decay(100.0, 8) > 100.0);
    assert!(rating.apply_rd_decay(100.0, 28) > rating.apply_rd_decay(100.0, 8));
}

#[derive(Clone)]
struct StaticRecordedDiscovery {
    outcome: RecordedMatchDiscoveryOutcome,
}

#[async_trait]
impl RecordedMatchDiscovery for StaticRecordedDiscovery {
    async fn discover_recorded_match(
        &self,
        _guild_id: i64,
        _match_id: i64,
    ) -> Result<RecordedMatchDiscoveryOutcome, String> {
        Ok(self.outcome.clone())
    }
}

#[derive(Default)]
struct PublicationDiscord {
    sent: Mutex<Vec<(u64, DiscordMessage)>>,
}

#[async_trait]
impl DiscordTransport for PublicationDiscord {
    async fn fetch_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
    ) -> Result<Option<crate::discord_transport::DiscordMessageSnapshot>, String> {
        Ok(None)
    }

    async fn send_message(
        &self,
        channel_id: u64,
        message: DiscordMessage,
    ) -> Result<crate::discord_transport::DiscordMessageReceipt, String> {
        self.sent
            .lock()
            .expect("publication capture")
            .push((channel_id, message));
        Ok(crate::discord_transport::DiscordMessageReceipt {
            channel_id,
            message_id: 1,
            jump_url: "https://discord.invalid/publication".to_owned(),
        })
    }

    async fn edit_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _message: DiscordMessage,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn delete_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn create_public_thread(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _name: &str,
    ) -> Result<u64, String> {
        Ok(1)
    }

    async fn pin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn archive_thread(
        &self,
        _thread_id: u64,
        _name: &str,
        _locked: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn add_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
        _user_id: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn clear_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn unpin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn send_direct_message(
        &self,
        _user_id: u64,
        _message: DiscordMessage,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn guild_member(
        &self,
        _guild_id: u64,
        _user_id: u64,
    ) -> Result<Option<crate::discord_transport::DiscordGuildMemberSnapshot>, String> {
        Ok(None)
    }
}

struct PublicationProbeDiscord {
    sent: Mutex<Vec<(u64, DiscordMessage)>>,
    thread_replies: Mutex<Vec<(u64, u64, DiscordMessage)>>,
    direct_messages: Mutex<Vec<(u64, DiscordMessage)>>,
    message_edits: Mutex<Vec<(u64, u64, DiscordMessage)>>,
    thread_edits: Mutex<Vec<(u64, String, bool, bool)>>,
    unpins: Mutex<Vec<(u64, u64)>>,
    order: Mutex<Vec<String>>,
    failed_send_channels: Mutex<BTreeSet<u64>>,
    fail_message_edits: AtomicBool,
    blocked_send_channels: Mutex<BTreeSet<u64>>,
    blocked_send_started: AtomicUsize,
    send_gate: tokio::sync::Semaphore,
    block_rename: AtomicBool,
    rename_started: AtomicBool,
    rename_gate: tokio::sync::Semaphore,
    block_unpin: AtomicBool,
    unpin_started: AtomicBool,
    unpin_gate: tokio::sync::Semaphore,
    next_message_id: AtomicU64,
    members: Mutex<BTreeMap<(u64, u64), crate::discord_transport::DiscordGuildMemberSnapshot>>,
    member_lookups: Mutex<Vec<(u64, u64)>>,
    cached_member_lookups: Mutex<Vec<(u64, u64)>>,
    failed_cached_member_lookups: Mutex<BTreeSet<(u64, u64)>>,
}

impl Default for PublicationProbeDiscord {
    fn default() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            thread_replies: Mutex::new(Vec::new()),
            direct_messages: Mutex::new(Vec::new()),
            message_edits: Mutex::new(Vec::new()),
            thread_edits: Mutex::new(Vec::new()),
            unpins: Mutex::new(Vec::new()),
            order: Mutex::new(Vec::new()),
            failed_send_channels: Mutex::new(BTreeSet::new()),
            fail_message_edits: AtomicBool::new(false),
            blocked_send_channels: Mutex::new(BTreeSet::new()),
            blocked_send_started: AtomicUsize::new(0),
            send_gate: tokio::sync::Semaphore::new(0),
            block_rename: AtomicBool::new(false),
            rename_started: AtomicBool::new(false),
            rename_gate: tokio::sync::Semaphore::new(0),
            block_unpin: AtomicBool::new(false),
            unpin_started: AtomicBool::new(false),
            unpin_gate: tokio::sync::Semaphore::new(0),
            next_message_id: AtomicU64::new(1),
            members: Mutex::new(BTreeMap::new()),
            member_lookups: Mutex::new(Vec::new()),
            cached_member_lookups: Mutex::new(Vec::new()),
            failed_cached_member_lookups: Mutex::new(BTreeSet::new()),
        }
    }
}

impl PublicationProbeDiscord {
    fn set_member(&self, guild_id: u64, user_id: u64, display_name: &str) {
        self.members.lock().expect("publication members").insert(
            (guild_id, user_id),
            crate::discord_transport::DiscordGuildMemberSnapshot {
                user_id,
                display_name: display_name.to_owned(),
                presence: crate::discord_transport::DiscordPresence::Unknown,
                in_voice: false,
                deafened: false,
                activities: Vec::new(),
            },
        );
    }

    fn member_lookups(&self) -> Vec<(u64, u64)> {
        self.member_lookups
            .lock()
            .expect("publication member lookups")
            .clone()
    }

    fn cached_member_lookups(&self) -> Vec<(u64, u64)> {
        self.cached_member_lookups
            .lock()
            .expect("publication cached member lookups")
            .clone()
    }

    fn fail_cached_member_lookup(&self, guild_id: u64, user_id: u64) {
        self.failed_cached_member_lookups
            .lock()
            .expect("failed publication cached member lookups")
            .insert((guild_id, user_id));
    }

    fn fail_sends_to(&self, channel_ids: impl IntoIterator<Item = u64>) {
        self.failed_send_channels
            .lock()
            .expect("failed publication channels")
            .extend(channel_ids);
    }

    fn block_sends_to(&self, channel_ids: impl IntoIterator<Item = u64>) {
        self.blocked_send_channels
            .lock()
            .expect("blocked publication channels")
            .extend(channel_ids);
    }

    fn sent_messages(&self) -> Vec<(u64, DiscordMessage)> {
        self.sent.lock().expect("publication messages").clone()
    }

    fn thread_replies(&self) -> Vec<(u64, u64, DiscordMessage)> {
        self.thread_replies
            .lock()
            .expect("publication thread replies")
            .clone()
    }

    fn direct_messages(&self) -> Vec<(u64, DiscordMessage)> {
        self.direct_messages
            .lock()
            .expect("publication direct messages")
            .clone()
    }

    fn message_edits(&self) -> Vec<(u64, u64, DiscordMessage)> {
        self.message_edits
            .lock()
            .expect("publication message edits")
            .clone()
    }

    fn thread_edits(&self) -> Vec<(u64, String, bool, bool)> {
        self.thread_edits
            .lock()
            .expect("publication thread edits")
            .clone()
    }

    fn unpins(&self) -> Vec<(u64, u64)> {
        self.unpins.lock().expect("publication unpins").clone()
    }

    fn order(&self) -> Vec<String> {
        self.order.lock().expect("publication order").clone()
    }
}

#[async_trait]
impl DiscordTransport for PublicationProbeDiscord {
    async fn fetch_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
    ) -> Result<Option<crate::discord_transport::DiscordMessageSnapshot>, String> {
        Ok(None)
    }

    async fn send_message(
        &self,
        channel_id: u64,
        message: DiscordMessage,
    ) -> Result<crate::discord_transport::DiscordMessageReceipt, String> {
        let kind = if message.response.embeds.is_empty() {
            "content"
        } else {
            "embed"
        };
        self.order
            .lock()
            .expect("publication order")
            .push(format!("send:{channel_id}:{kind}:started"));
        self.sent
            .lock()
            .expect("publication messages")
            .push((channel_id, message));
        if self
            .failed_send_channels
            .lock()
            .expect("failed publication channels")
            .contains(&channel_id)
        {
            self.order
                .lock()
                .expect("publication order")
                .push(format!("send:{channel_id}:{kind}:failed"));
            return Err(format!("channel {channel_id} unavailable"));
        }
        let blocked = self
            .blocked_send_channels
            .lock()
            .expect("blocked publication channels")
            .contains(&channel_id);
        if blocked {
            self.blocked_send_started.fetch_add(1, Ordering::AcqRel);
            self.send_gate
                .acquire()
                .await
                .map_err(|_| "publication send gate closed".to_owned())?
                .forget();
        }
        self.order
            .lock()
            .expect("publication order")
            .push(format!("send:{channel_id}:{kind}:done"));
        let message_id = self.next_message_id.fetch_add(1, Ordering::AcqRel);
        Ok(crate::discord_transport::DiscordMessageReceipt {
            channel_id,
            message_id,
            jump_url: format!("https://discord.invalid/{channel_id}/{message_id}"),
        })
    }

    async fn send_thread_reply(
        &self,
        thread_id: u64,
        parent_message_id: u64,
        message: DiscordMessage,
    ) -> Result<crate::discord_transport::DiscordMessageReceipt, String> {
        self.thread_replies
            .lock()
            .expect("publication thread replies")
            .push((thread_id, parent_message_id, message.clone()));
        DiscordTransport::send_message(self, thread_id, message).await
    }

    async fn edit_message(
        &self,
        channel_id: u64,
        message_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.message_edits
            .lock()
            .expect("publication message edits")
            .push((channel_id, message_id, message));
        if self.fail_message_edits.load(Ordering::Acquire) {
            return Err("simulated lobby display failure".to_owned());
        }
        Ok(())
    }

    async fn delete_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn create_public_thread(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _name: &str,
    ) -> Result<u64, String> {
        Ok(1)
    }

    async fn pin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn archive_thread(&self, thread_id: u64, name: &str, locked: bool) -> Result<(), String> {
        self.thread_edits
            .lock()
            .expect("publication thread edits")
            .push((thread_id, name.to_owned(), true, locked));
        Ok(())
    }

    async fn edit_thread(
        &self,
        thread_id: u64,
        name: &str,
        archived: bool,
        locked: bool,
    ) -> Result<(), String> {
        let phase = if archived {
            "archive"
        } else if locked {
            "lock"
        } else {
            "rename"
        };
        self.order
            .lock()
            .expect("publication order")
            .push(format!("thread:{phase}:started"));
        self.thread_edits
            .lock()
            .expect("publication thread edits")
            .push((thread_id, name.to_owned(), archived, locked));
        if phase == "rename" && self.block_rename.load(Ordering::Acquire) {
            self.rename_started.store(true, Ordering::Release);
            self.rename_gate
                .acquire()
                .await
                .map_err(|_| "publication rename gate closed".to_owned())?
                .forget();
        }
        self.order
            .lock()
            .expect("publication order")
            .push(format!("thread:{phase}:done"));
        Ok(())
    }

    async fn add_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
        _user_id: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn clear_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn unpin_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        self.order
            .lock()
            .expect("publication order")
            .push("unpin:started".to_owned());
        self.unpins
            .lock()
            .expect("publication unpins")
            .push((channel_id, message_id));
        if self.block_unpin.load(Ordering::Acquire) {
            self.unpin_started.store(true, Ordering::Release);
            self.unpin_gate
                .acquire()
                .await
                .map_err(|_| "publication unpin gate closed".to_owned())?
                .forget();
        }
        self.order
            .lock()
            .expect("publication order")
            .push("unpin:done".to_owned());
        Ok(())
    }

    async fn send_direct_message(
        &self,
        user_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.direct_messages
            .lock()
            .expect("publication direct messages")
            .push((user_id, message));
        Ok(())
    }

    async fn guild_member(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<Option<crate::discord_transport::DiscordGuildMemberSnapshot>, String> {
        self.member_lookups
            .lock()
            .expect("publication member lookups")
            .push((guild_id, user_id));
        Ok(self
            .members
            .lock()
            .expect("publication members")
            .get(&(guild_id, user_id))
            .cloned())
    }

    fn cached_guild_member_display_name(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<Option<String>, String> {
        self.cached_member_lookups
            .lock()
            .expect("publication cached member lookups")
            .push((guild_id, user_id));
        if self
            .failed_cached_member_lookups
            .lock()
            .expect("failed publication cached member lookups")
            .contains(&(guild_id, user_id))
        {
            return Err("cached member lookup failed".to_owned());
        }
        Ok(self
            .members
            .lock()
            .expect("publication members")
            .get(&(guild_id, user_id))
            .map(|member| member.display_name.clone()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_match_completion_events_skip_lowprio_dms_but_notify_suspensions() {
    use cama_db::moderation::{ModerationSource, RecordModerationEventRequest};

    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let repository = ModerationRepository::new(fixture.database.path());
    for (discord_id, event_type, match_id) in [
        (101, ModerationEventType::LowprioComplete, 314),
        (202, ModerationEventType::Complete, 314),
        (303, ModerationEventType::LowprioComplete, 315),
        (404, ModerationEventType::Kick, 314),
    ] {
        repository
            .record_event(RecordModerationEventRequest {
                discord_id,
                guild_id: Some(GUILD),
                event_type,
                source: ModerationSource::System,
                actor_id: None,
                reason: None,
                scope: None,
                completion: None,
                expires_at: None,
                matches_required: None,
                matches_remaining: None,
                pending_match_watermark: None,
                wins_required: None,
                wins_remaining: None,
                match_id: Some(match_id),
                now: 1_700_000_000,
            })
            .expect("insert moderation completion fixture");
    }

    fixture
        .provider
        .handler
        .notify_moderation_completions(GUILD, 314)
        .await;

    let mut direct_messages = discord.direct_messages();
    direct_messages.sort_by_key(|(discord_id, _)| *discord_id);
    assert_eq!(
        direct_messages
            .iter()
            .map(|(discord_id, _)| *discord_id)
            .collect::<Vec<_>>(),
        vec![202]
    );
    assert!(
        direct_messages[0]
            .1
            .response
            .content
            .contains("lobby suspension")
    );
    assert!(
        direct_messages
            .iter()
            .all(|(_, message)| message.response.content.contains("#314"))
    );
    assert!(direct_messages.iter().all(|(_, message)| {
        !message
            .response
            .content
            .contains("low-priority win requirement")
    }));
}

fn settled_bet(
    bet_id: i64,
    discord_id: i64,
    team: BettingTeam,
    effective_amount: i64,
    payout: i64,
) -> cama_db::betting_service_repository::SettledBet {
    cama_db::betting_service_repository::SettledBet {
        bet_id,
        discord_id,
        team,
        effective_amount,
        payout,
        balance_after: 0,
    }
}

#[test]
fn test_bet_jc_deltas_net_all_stakes_payouts_and_deductions_per_user() {
    let settlement = SettlementResult {
        winners: vec![settled_bet(1, 1, BettingTeam::Radiant, 10, 20)],
        losers: vec![
            settled_bet(2, 1, BettingTeam::Dire, 4, 0),
            settled_bet(3, 2, BettingTeam::Dire, 5, 0),
        ],
        bankruptcy_penalties: BTreeMap::from([(1, 2)]),
        vanity_taxes: BTreeMap::from([(1, 1)]),
        ..SettlementResult::default()
    };
    assert_eq!(
        bet_jc_deltas(&settlement, &BTreeMap::from([(1, 1)])),
        BTreeMap::from([(1, 2), (2, -5)])
    );
}

#[test]
fn test_jc_changes_format_groups_players_and_bettors_sorted_by_net_change() {
    let settlement = SettlementResult {
        winners: vec![
            settled_bet(1, 6, BettingTeam::Radiant, 4, 10),
            settled_bet(2, 1, BettingTeam::Radiant, 4, 10),
        ],
        losers: vec![
            settled_bet(3, 2, BettingTeam::Dire, 10, 0),
            settled_bet(4, 3, BettingTeam::Dire, 3, 0),
            settled_bet(5, 5, BettingTeam::Dire, 7, 0),
            settled_bet(6, 6, BettingTeam::Dire, 2, 0),
            settled_bet(7, 1, BettingTeam::Dire, 1, 0),
        ],
        ..SettlementResult::default()
    };
    let changes = BTreeMap::from([
        (5, BTreeMap::from([("bet".to_owned(), -7)])),
        (6, BTreeMap::from([("bet".to_owned(), 9)])),
        (
            3,
            BTreeMap::from([("payout".to_owned(), 5), ("bet".to_owned(), -3)]),
        ),
        (
            1,
            BTreeMap::from([
                ("payout".to_owned(), 10),
                ("bet".to_owned(), 5),
                ("bet_blood_pact_skim".to_owned(), 1),
            ]),
        ),
        (4, BTreeMap::from([("payout".to_owned(), 5)])),
        (
            2,
            BTreeMap::from([
                ("payout".to_owned(), 10),
                ("bet".to_owned(), -10),
                ("streak".to_owned(), 1),
            ]),
        ),
    ]);
    assert_eq!(
        render_jc_lines(&[1, 2], &[3], &[4], &changes, &settlement).join("\n"),
        format!(
            "**Final odds:** 🟢 Radiant 2.50x\n\
             **Match Players:**\n\
             <@1>: **+15** {JOPACOIN_EMOTE} (win +10, bet +5 · stake 5)\n\
             <@4>: **+5** {JOPACOIN_EMOTE} (excluded +5)\n\
             <@3>: **+2** {JOPACOIN_EMOTE} (play +5, bet −3 · stake 3)\n\
             <@2>: **+1** {JOPACOIN_EMOTE} (win +10, bet −10 · stake 10, streak +1)\n\
             **Bettors:**\n\
             <@6>: **+9** {JOPACOIN_EMOTE} (bet +9 · stake 6)\n\
             <@5>: **−7** {JOPACOIN_EMOTE} (bet −7 · stake 7)"
        )
    );
}

#[test]
fn test_jc_changes_aggregates_multiple_effective_stakes_per_bettor() {
    let settlement = SettlementResult {
        winners: vec![
            settled_bet(1, 7, BettingTeam::Dire, 20, 35),
            settled_bet(2, 7, BettingTeam::Dire, 20, 35),
        ],
        ..SettlementResult::default()
    };
    let changes = BTreeMap::from([(7, BTreeMap::from([("bet".to_owned(), 30)]))]);
    assert_eq!(
        render_jc_lines(&[], &[], &[], &changes, &settlement).join("\n"),
        format!(
            "**Final odds:** 🔴 Dire 1.75x\n\
             **Bettors:**\n\
             <@7>: **+30** {JOPACOIN_EMOTE} (bet +30 · stake 40)"
        )
    );
}

struct FailingConfirmationResponder {
    started: AtomicBool,
}

#[async_trait]
impl InteractionResponder for FailingConfirmationResponder {
    async fn respond(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        Ok(())
    }

    async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
        Ok(())
    }

    async fn followup(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.started.store(true, Ordering::Release);
        Err(InteractionResponseError::new("confirmation failed"))
    }
}

fn publication_snapshot(lobby_kind: LobbyKind, thread_id: Option<u64>) -> MatchLobbySnapshot {
    MatchLobbySnapshot {
        guild_id: GUILD,
        lobby_kind,
        created_by: None,
        player_ids: Vec::new(),
        player_join_times: BTreeMap::new(),
        confirmed_player_ids: None,
        ready_threshold: 10,
        lobby_channel_id: Some(100),
        lobby_message_id: Some(321),
        origin_channel_id: Some(200),
        thread_id,
    }
}

fn shuffle_context(channel_id: Option<u64>) -> MatchCommandContext {
    MatchCommandContext {
        user_id: 99,
        guild_id: GUILD,
        channel_id,
        member_permissions: Some(ADMINISTRATOR_PERMISSION),
        options: Vec::new(),
    }
}

fn shuffle_command_context(user_id: i64, lobby_kind: LobbyKind) -> MatchCommandContext {
    MatchCommandContext {
        user_id,
        guild_id: GUILD,
        channel_id: Some(77_001),
        member_permissions: None,
        options: vec![InteractionOption {
            name: "lobby".to_owned(),
            value: InteractionValue::String(lobby_kind_value(lobby_kind).to_owned()),
        }],
    }
}

fn inferred_shuffle_command_context(user_id: i64) -> MatchCommandContext {
    MatchCommandContext {
        user_id,
        guild_id: GUILD,
        channel_id: Some(77_001),
        member_permissions: None,
        options: Vec::new(),
    }
}

fn roster_snapshot(
    player_ids: Vec<i64>,
    confirmed_player_ids: Option<BTreeSet<i64>>,
    ready_threshold: usize,
) -> MatchLobbySnapshot {
    MatchLobbySnapshot {
        guild_id: GUILD,
        lobby_kind: LobbyKind::Open,
        created_by: player_ids.first().copied(),
        player_ids,
        player_join_times: BTreeMap::new(),
        confirmed_player_ids,
        ready_threshold,
        lobby_channel_id: None,
        lobby_message_id: None,
        origin_channel_id: None,
        thread_id: None,
    }
}

async fn wait_for_count(counter: &AtomicUsize, expected: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while counter.load(Ordering::Acquire) < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("publication operation started");
}

async fn wait_for_sent(discord: &PublicationProbeDiscord, expected: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while discord.sent_messages().len() < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("publication messages sent");
}

#[test]
fn test_shuffle_roster_ignores_legacy_conditional_players() {
    let regular_ids = (1..10).collect::<Vec<_>>();
    let snapshot = roster_snapshot(regular_ids.clone(), None, 10);

    let (selected, excluded) = selected_roster(&snapshot);

    assert_eq!(selected, regular_ids);
    assert!(excluded.is_empty());
}

#[test]
fn test_readycheck_nonresponders_are_implicit_conditional_exclusions() {
    let player_ids = (1..16).collect::<Vec<_>>();
    let confirmed = player_ids[..10]
        .iter()
        .copied()
        .chain([999])
        .collect::<BTreeSet<_>>();
    let snapshot = roster_snapshot(player_ids.clone(), Some(confirmed), 10);

    let (selected, excluded) = selected_roster(&snapshot);

    assert_eq!(selected, player_ids[..10]);
    assert_eq!(excluded, player_ids[10..]);
}

#[test]
fn test_readycheck_below_threshold_does_not_change_shuffle_roster() {
    let player_ids = (1..13).collect::<Vec<_>>();
    let confirmed = player_ids[..9]
        .iter()
        .copied()
        .chain([999])
        .collect::<BTreeSet<_>>();
    let snapshot = roster_snapshot(player_ids.clone(), Some(confirmed), 10);

    let (selected, excluded) = selected_roster(&snapshot);

    assert_eq!(selected, player_ids);
    assert!(excluded.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_execute_shuffle_stops_if_lobby_changes_before_atomic_snapshot() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.set_glicko_rating(&player_ids, 1_300.0);
    fixture.populate_lobby(&player_ids, LobbyKind::Open).await;
    let reserved = player_ids.iter().copied().collect::<BTreeSet<_>>();
    assert!(
        fixture
            .provider
            .handler
            .lobbies
            .reserve_players(GUILD, LobbyKind::Open, &reserved)
    );
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .handle_shuffle(
            shuffle_command_context(player_ids[0], LobbyKind::Open),
            responder.clone(),
        )
        .await
        .expect("reservation race is a friendly response");

    assert_eq!(
        responder.contents(),
        ["❌ The lobby changed while starting the shuffle. Please try again."]
    );
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_matches(GUILD)
            .expect("read reservation-race state")
            .is_empty()
    );
    fixture
        .provider
        .handler
        .lobbies
        .release_players(GUILD, LobbyKind::Open, &reserved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shuffle_preconditions_reject_legacy_conditional_lobby_member() {
    let fixture = MatchRuntimeFixture::new();
    let responder = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_shuffle(inferred_shuffle_command_context(99), responder.clone())
        .await
        .expect("conditional non-member is rejected cleanly");
    assert_eq!(
        responder.contents(),
        ["❌ Join a lobby before using `/shuffle`."]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shuffle_preconditions_reject_admin_outside_lobby() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.populate_lobby(&player_ids, LobbyKind::Open).await;
    let responder = Arc::new(RecordingMatchResponder::default());
    let mut context = shuffle_command_context(99, LobbyKind::Open);
    context.member_permissions = Some(ADMINISTRATOR_PERMISSION);
    fixture
        .provider
        .handler
        .handle_shuffle(context, responder.clone())
        .await
        .expect("admin outside lobby is rejected cleanly");
    assert_eq!(
        responder.contents(),
        ["❌ Join 🍽️ All You Can Feed before using `/shuffle`."]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shuffle_preconditions_reject_non_admin_outside_lobby() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.populate_lobby(&player_ids, LobbyKind::Open).await;
    let responder = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_shuffle(
            shuffle_command_context(99, LobbyKind::Open),
            responder.clone(),
        )
        .await
        .expect("non-admin outside lobby is rejected cleanly");
    assert_eq!(
        responder.contents(),
        ["❌ Join 🍽️ All You Can Feed before using `/shuffle`."]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shuffle_command_requires_a_lobby_when_queued_in_both() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.set_glicko_rating(&player_ids, 1_300.0);
    fixture.populate_lobby(&player_ids, LobbyKind::Open).await;
    fixture
        .populate_lobby(&player_ids, LobbyKind::LowSkill)
        .await;
    let responder = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_shuffle(
            inferred_shuffle_command_context(player_ids[0]),
            responder.clone(),
        )
        .await
        .expect("ambiguous lobby is rejected cleanly");
    assert_eq!(responder.contents(), [AMBIGUOUS_LOBBY_MESSAGE]);
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_matches(GUILD)
            .expect("read ambiguous shuffle state")
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shuffle_preconditions_allow_regular_lobby_member() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.populate_lobby(&player_ids, LobbyKind::Open).await;
    let responder = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_shuffle(
            inferred_shuffle_command_context(player_ids[0]),
            responder.clone(),
        )
        .await
        .expect("regular member shuffles");
    assert!(
        responder
            .contents()
            .iter()
            .any(|content| content == "✅ 🍽️ All You Can Feed teams shuffled!")
    );
    let pending = PendingMatchRepository::new(fixture.database.path())
        .single_pending_match(GUILD)
        .expect("read regular shuffle")
        .expect("regular shuffle persists");
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shuffle_preconditions_reject_player_already_in_pending_match() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.populate_lobby(&player_ids, LobbyKind::Open).await;
    let mut pending_state = PendingMatchState {
        radiant_team_ids: vec![player_ids[0]],
        shuffle_message_jump_url: Some("https://discord.com/channels/1/2/3".to_owned()),
        ..PendingMatchState::default()
    };
    pending_state.lobby_kind = Some("open".to_owned());
    let pending = PendingMatchRepository::new(fixture.database.path())
        .create_pending_match(GUILD, &pending_state)
        .expect("seed existing pending match");
    let responder = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_shuffle(
            inferred_shuffle_command_context(player_ids[0]),
            responder.clone(),
        )
        .await
        .expect("pending participant is rejected cleanly");
    assert_eq!(
        responder.contents(),
        [format!(
            "❌ You're already in a pending match (Match #{})! [View your match](https://discord.com/channels/1/2/3) and use `/record` to complete it first.",
            pending.pending_match_id
        )]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_active_draft_only_blocks_its_source_lobby() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.set_glicko_rating(&player_ids, 1_300.0);
    fixture
        .populate_lobby(&player_ids, LobbyKind::LowSkill)
        .await;
    fixture
        .drafts
        .create_draft(Some(GUILD), cama_app::draft::LobbyKind::Open)
        .expect("seed open-lobby draft");
    let responder = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_shuffle(
            shuffle_command_context(player_ids[0], LobbyKind::LowSkill),
            responder.clone(),
        )
        .await
        .expect("other lobby remains shuffleable");
    assert!(
        responder
            .contents()
            .iter()
            .any(|content| content == "✅ 🧀 Whine & Cheese teams shuffled!")
    );
    let pending = PendingMatchRepository::new(fixture.database.path())
        .single_pending_match(GUILD)
        .expect("read other-lobby shuffle")
        .expect("other-lobby shuffle persists");
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shuffle_command_infers_lowskill_membership() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.set_glicko_rating(&player_ids, 1_300.0);
    fixture
        .populate_lobby(&player_ids, LobbyKind::LowSkill)
        .await;
    let responder = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_shuffle(
            inferred_shuffle_command_context(player_ids[0]),
            responder.clone(),
        )
        .await
        .expect("infer low-skill lobby");
    assert!(
        responder
            .contents()
            .iter()
            .any(|content| content == "✅ 🧀 Whine & Cheese teams shuffled!")
    );
    let pending = PendingMatchRepository::new(fixture.database.path())
        .single_pending_match(GUILD)
        .expect("read inferred shuffle")
        .expect("inferred shuffle persists");
    assert_eq!(pending.state.lobby_kind.as_deref(), Some("lowskill"));
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shuffle_command_honours_explicit_lobby_when_queued_in_both() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.set_glicko_rating(&player_ids, 1_300.0);
    fixture.populate_lobby(&player_ids, LobbyKind::Open).await;
    fixture
        .populate_lobby(&player_ids, LobbyKind::LowSkill)
        .await;
    let responder = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_shuffle(
            shuffle_command_context(player_ids[0], LobbyKind::LowSkill),
            responder.clone(),
        )
        .await
        .expect("explicit lobby resolves ambiguity");
    assert!(
        responder
            .contents()
            .iter()
            .any(|content| content == "✅ 🧀 Whine & Cheese teams shuffled!")
    );
    let pending = PendingMatchRepository::new(fixture.database.path())
        .single_pending_match(GUILD)
        .expect("read explicit shuffle")
        .expect("explicit shuffle persists");
    assert_eq!(pending.state.lobby_kind.as_deref(), Some("lowskill"));
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_active_draft_admin_hint_uses_central_admin_permission() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.populate_lobby(&player_ids, LobbyKind::Open).await;
    let draft = fixture
        .drafts
        .create_draft(Some(GUILD), cama_app::draft::LobbyKind::Open)
        .expect("seed active draft");
    draft.lock().expect("draft state").captain1_id = Some(50);
    let mut context = shuffle_command_context(42, LobbyKind::Open);
    context.member_permissions = Some(MANAGE_GUILD_PERMISSION);
    let responder = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .handle_shuffle(context, responder.clone())
        .await
        .expect("active draft hint is friendly");
    let content = responder.contents().join("\n");
    assert!(content.contains("/draft restart"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_execute_shuffle_passes_no_conditional_exclusions_to_match_service() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.populate_lobby(&player_ids, LobbyKind::Open).await;
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .handle_shuffle(
            shuffle_command_context(player_ids[0], LobbyKind::Open),
            responder,
        )
        .await
        .expect("shuffle regular roster");

    let pending = PendingMatchRepository::new(fixture.database.path())
        .pending_matches(GUILD)
        .expect("read regular shuffle")
        .into_iter()
        .next()
        .expect("regular shuffle persisted");
    assert!(pending.state.excluded_conditional_player_ids.is_empty());
    assert!(pending.state.half_exclusion_increment_ids.is_empty());

    let mut wait_snapshot = roster_snapshot(vec![100, 101, 102], None, 10);
    wait_snapshot.player_join_times = BTreeMap::from([
        (100, 10_000.0 - (30.0 * 60.0 + 59.0)),
        (101, 10_000.0 - 59.0),
        (102, 10_000.0 + 60.0),
    ]);
    assert_eq!(
        lobby_wait_minutes(&wait_snapshot, &[100, 101, 102], 10_000),
        HashMap::from([(100, 30), (101, 0), (102, 0)])
    );
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_execute_shuffle_refreshes_pool_exactly_once_on_later_failure() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.populate_lobby(&player_ids, LobbyKind::Open).await;
    let baseline_edits = discord.message_edits().len();
    let responder: Arc<dyn InteractionResponder> = Arc::new(FailingConfirmationResponder {
        started: AtomicBool::new(false),
    });

    let result = fixture
        .provider
        .handler
        .handle_shuffle(
            shuffle_command_context(player_ids[0], LobbyKind::Open),
            responder,
        )
        .await;

    assert!(result.is_err());
    assert_eq!(discord.message_edits().len(), baseline_edits + 1);
    let pending = PendingMatchRepository::new(fixture.database.path())
        .pending_matches(GUILD)
        .expect("read failed-finalize shuffle")
        .into_iter()
        .next()
        .expect("failed-finalize pending remains");
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_execute_shuffle_refreshes_pool_exactly_once_after_finalize() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let source_ids = fixture.add_shuffle_pool(10, false);
    let sibling_ids = fixture.add_shuffle_pool_from(85_000, 1, false);
    fixture.set_glicko_rating(&sibling_ids, 1_300.0);
    fixture.populate_lobby(&source_ids, LobbyKind::Open).await;
    fixture
        .populate_lobby(&sibling_ids, LobbyKind::LowSkill)
        .await;
    let baseline_edits = discord.message_edits().len();

    fixture
        .provider
        .handler
        .handle_shuffle(
            shuffle_command_context(source_ids[0], LobbyKind::Open),
            Arc::new(RecordingMatchResponder::default()),
        )
        .await
        .expect("finalize source shuffle");

    assert_eq!(discord.message_edits().len(), baseline_edits + 1);
    assert!(
        fixture
            .provider
            .handler
            .lobbies
            .snapshot(GUILD, LobbyKind::Open)
            .is_none()
    );
    assert_eq!(
        fixture
            .provider
            .handler
            .lobbies
            .snapshot(GUILD, LobbyKind::LowSkill)
            .expect("sibling survives refresh")
            .player_ids,
        sibling_ids
    );
    let pending = PendingMatchRepository::new(fixture.database.path())
        .pending_matches(GUILD)
        .expect("read finalized shuffle")
        .into_iter()
        .next()
        .expect("finalized pending exists");
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_execute_shuffle_evicts_only_the_matched_players_before_releasing() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(15, false);
    fixture.set_glicko_rating(&player_ids, 1_300.0);
    fixture.populate_lobby(&player_ids, LobbyKind::Open).await;
    fixture
        .populate_lobby(&player_ids, LobbyKind::LowSkill)
        .await;

    fixture
        .provider
        .handler
        .handle_shuffle(
            shuffle_command_context(player_ids[0], LobbyKind::LowSkill),
            Arc::new(RecordingMatchResponder::default()),
        )
        .await
        .expect("shuffle dual-queued roster");

    let pending = PendingMatchRepository::new(fixture.database.path())
        .pending_matches(GUILD)
        .expect("read dual-queue shuffle")
        .into_iter()
        .next()
        .expect("dual-queue pending exists");
    assert_eq!(pending.state.participant_ids().len(), 10);
    let sibling = fixture
        .provider
        .handler
        .lobbies
        .snapshot(GUILD, LobbyKind::Open)
        .expect("sibling lobby survives");
    assert_eq!(
        sibling.player_ids.into_iter().collect::<BTreeSet<_>>(),
        pending
            .state
            .excluded_player_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
    );
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_eviction_failure_does_not_abort_the_started_shuffle() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.set_glicko_rating(&player_ids, 1_300.0);
    fixture.populate_lobby(&player_ids, LobbyKind::Open).await;
    fixture
        .populate_lobby(&player_ids, LobbyKind::LowSkill)
        .await;
    discord.fail_message_edits.store(true, Ordering::Release);
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .handle_shuffle(
            shuffle_command_context(player_ids[0], LobbyKind::LowSkill),
            responder.clone(),
        )
        .await
        .expect("display failure cannot unwind persisted shuffle");

    assert!(
        responder
            .contents()
            .iter()
            .any(|content| content == "✅ 🧀 Whine & Cheese teams shuffled!")
    );
    let pending = PendingMatchRepository::new(fixture.database.path())
        .pending_matches(GUILD)
        .expect("read shuffle after display failure")
        .into_iter()
        .next()
        .expect("shuffle remains persisted");
    assert_eq!(pending.state.participant_ids().len(), 10);
    abort_betting_tasks(&fixture, pending.pending_match_id);
}

#[test]
fn test_readycheck_nonresponders_are_forwarded_to_normal_shuffle() {
    let fixture = MatchRuntimeFixture::new();
    let all_ids = fixture.add_shuffle_pool(15, false);
    let snapshot = roster_snapshot(
        all_ids.clone(),
        Some(all_ids[..10].iter().copied().collect()),
        10,
    );
    let (selected, nonresponders) = selected_roster(&snapshot);

    let prepared = fixture.prepare_shuffle(selected, "glicko", nonresponders.clone());

    assert_eq!(
        prepared.pending.state.excluded_conditional_player_ids,
        nonresponders
    );
    assert_eq!(
        prepared.pending.state.half_exclusion_increment_ids,
        all_ids[10..]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_record_without_shuffle_raises() {
    let fixture = MatchRuntimeFixture::new();
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .handle_record(
            record_context(1, GUILD, "radiant", false),
            responder.clone(),
        )
        .await
        .expect("missing shuffle is a friendly response");

    assert_eq!(responder.contents(), ["❌ No pending match to record."]);
}

#[test]
fn test_record_with_invalid_winning_team_raises() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);

    let error = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "spectator", None)
        .expect_err("unknown winner must be rejected");

    assert_eq!(error, "winning_team must be 'radiant' or 'dire'.");
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read rejected record")
            .is_some()
    );
}

#[test]
fn test_excluded_player_on_a_team_aborts_recording() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let repository = PendingMatchRepository::new(fixture.database.path());
    let pending = repository
        .mutate_pending_match(GUILD, pending.pending_match_id, |state| {
            state.excluded_player_ids = vec![state.radiant_team_ids[0]];
        })
        .expect("persist corrupt excluded state")
        .expect("pending state exists")
        .0;

    let error = fixture
        .provider
        .handler
        .record_match_blocking(&pending, "radiant", None)
        .expect_err("excluded participant must fail closed");

    assert_eq!(error, "Excluded players detected in match teams.");
    let connection = Connection::open(fixture.database.path()).expect("inspect failed record");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM matches", [], |row| row
                .get::<_, i64>(0))
            .expect("count failed records"),
        0
    );
    assert!(
        repository
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read corrupt pending state")
            .is_some()
    );
}

#[test]
fn test_clean_shuffle_excluded_set_disjoint_from_teams() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(14, false);

    let prepared = fixture.prepare_shuffle(player_ids.clone(), "glicko", Vec::new());
    let participants = prepared.pending.state.participant_ids();
    let excluded = prepared
        .pending
        .state
        .excluded_player_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();

    assert_eq!(participants.len(), 10);
    assert_eq!(excluded.len(), 4);
    assert!(participants.is_disjoint(&excluded));
    assert_eq!(
        participants
            .union(&excluded)
            .copied()
            .collect::<BTreeSet<_>>(),
        player_ids.into_iter().collect()
    );
}

#[tokio::test]
async fn test_shuffle_embed_uses_server_display_names_without_http_lookup() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let player_ids = fixture.add_shuffle_pool(14, false);
    let prepared = fixture.prepare_shuffle(player_ids, "glicko", Vec::new());
    let participant_id = prepared.pending.state.radiant_team_ids[0];
    let excluded_id = prepared.pending.state.excluded_player_ids[0];
    discord.set_member(
        u64::try_from(GUILD).expect("fixture guild ID"),
        u64::try_from(participant_id).expect("fixture player ID"),
        "Participant Server Name",
    );
    discord.set_member(
        u64::try_from(GUILD).expect("fixture guild ID"),
        u64::try_from(excluded_id).expect("fixture player ID"),
        "Excluded Server Name",
    );

    let embed = fixture
        .provider
        .handler
        .render_shuffle_embed(&prepared.pending)
        .await
        .expect("render shuffle embed");
    let teams = embed
        .fields
        .iter()
        .find(|field| field.name.contains("Radiant") && field.name.contains("Dire"))
        .expect("teams field");
    let balance = embed
        .fields
        .iter()
        .find(|field| field.name == "📊 Balance")
        .expect("balance field");

    assert!(teams.value.contains("Participant Server Name"));
    assert!(!teams.value.contains(&format!("shuffle-{participant_id}")));
    assert!(balance.value.contains("**Excluded:**"));
    assert!(balance.value.contains("Excluded Server Name"));
    assert!(!balance.value.contains(&format!("Unknown({excluded_id})")));
    assert!(discord.member_lookups().is_empty());
    let expected_lookups = prepared
        .pending
        .state
        .participant_ids()
        .into_iter()
        .chain(prepared.pending.state.excluded_player_ids.iter().copied())
        .map(|player_id| {
            (
                u64::try_from(GUILD).expect("fixture guild ID"),
                u64::try_from(player_id).expect("fixture player ID"),
            )
        })
        .collect::<BTreeSet<_>>();
    let cached_lookups = discord.cached_member_lookups();
    assert_eq!(cached_lookups.len(), expected_lookups.len());
    assert_eq!(
        cached_lookups.into_iter().collect::<BTreeSet<_>>(),
        expected_lookups
    );
}

#[tokio::test]
async fn test_shuffle_embed_uses_cached_name_when_player_row_is_missing() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let player_ids = fixture.add_shuffle_pool(14, false);
    let prepared = fixture.prepare_shuffle(player_ids, "glicko", Vec::new());
    let participant_id = prepared.pending.state.radiant_team_ids[0];
    discord.set_member(
        u64::try_from(GUILD).expect("fixture guild ID"),
        u64::try_from(participant_id).expect("fixture player ID"),
        "Deleted Player Server Name",
    );
    assert!(
        PlayerRepository::new(fixture.database.path())
            .delete(participant_id, Some(GUILD))
            .expect("delete player row")
    );

    let embed = fixture
        .provider
        .handler
        .render_shuffle_embed(&prepared.pending)
        .await
        .expect("render shuffle embed");
    let teams = embed
        .fields
        .iter()
        .find(|field| field.name.contains("Radiant") && field.name.contains("Dire"))
        .expect("teams field");

    assert!(teams.value.contains("Deleted Player Server Name"));
    assert!(!teams.value.contains(&format!("Unknown({participant_id})")));
    assert!(discord.member_lookups().is_empty());
}

#[tokio::test]
async fn test_shuffle_embed_falls_back_to_stored_name_on_cached_lookup_error() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let player_ids = fixture.add_shuffle_pool(14, false);
    let prepared = fixture.prepare_shuffle(player_ids, "glicko", Vec::new());
    let participant_id = prepared.pending.state.radiant_team_ids[0];
    discord.fail_cached_member_lookup(
        u64::try_from(GUILD).expect("fixture guild ID"),
        u64::try_from(participant_id).expect("fixture player ID"),
    );

    let embed = fixture
        .provider
        .handler
        .render_shuffle_embed(&prepared.pending)
        .await
        .expect("render shuffle embed");
    let teams = embed
        .fields
        .iter()
        .find(|field| field.name.contains("Radiant") && field.name.contains("Dire"))
        .expect("teams field");

    assert!(teams.value.contains(&format!("shuffle-{participant_id}")));
    assert!(!teams.value.contains(&format!("Unknown({participant_id})")));
    assert!(discord.member_lookups().is_empty());
}

#[tokio::test]
async fn test_shuffle_embed_falls_back_to_stored_name_for_excluded_player() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let player_ids = fixture.add_shuffle_pool(14, false);
    let prepared = fixture.prepare_shuffle(player_ids, "glicko", Vec::new());
    let excluded_id = prepared.pending.state.excluded_player_ids[0];

    let embed = fixture
        .provider
        .handler
        .render_shuffle_embed(&prepared.pending)
        .await
        .expect("render shuffle embed");
    let balance = embed
        .fields
        .iter()
        .find(|field| field.name == "📊 Balance")
        .expect("balance field");

    assert!(balance.value.contains(&format!("shuffle-{excluded_id}")));
    assert!(!balance.value.contains(&format!("Unknown({excluded_id})")));
    assert!(discord.member_lookups().is_empty());
}

#[tokio::test]
async fn test_shuffle_embed_skips_discord_lookup_for_invalid_excluded_ids() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let players = PlayerRepository::new(fixture.database.path());
    players
        .add(&NewPlayer::new(0, "zero-id-player", Some(GUILD)))
        .expect("add zero-ID player");
    players
        .add(&NewPlayer::new(-1, "negative-id-player", Some(GUILD)))
        .expect("add negative-ID player");
    let player_ids = fixture.add_shuffle_pool(10, false);
    let mut prepared = fixture.prepare_shuffle(player_ids, "glicko", Vec::new());
    prepared.pending.state.excluded_player_ids = vec![0, -1];

    let embed = fixture
        .provider
        .handler
        .render_shuffle_embed(&prepared.pending)
        .await
        .expect("render shuffle embed");
    let balance = embed
        .fields
        .iter()
        .find(|field| field.name == "📊 Balance")
        .expect("balance field");

    assert!(balance.value.contains("zero-id-player"));
    assert!(balance.value.contains("negative-id-player"));
    assert!(discord.member_lookups().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_execute_shuffle_reuses_loaded_roster_for_pending_names() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    fixture.populate_lobby(&player_ids, LobbyKind::Open).await;
    PendingMatchRepository::new(fixture.database.path())
        .create_pending_match(
            GUILD,
            &PendingMatchState {
                radiant_team_ids: vec![player_ids[1]],
                ..PendingMatchState::default()
            },
        )
        .expect("create blocking pending match");
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .handle_shuffle(
            shuffle_command_context(player_ids[0], LobbyKind::Open),
            responder.clone(),
        )
        .await
        .expect("blocked roster is a friendly response");

    let content = responder.contents().join("\n");
    assert!(content.contains("Cannot shuffle: 1 players"));
    assert!(content.contains(&format!("shuffle-{}", player_ids[1])));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_publication_sends_overlap_and_confirmation_error_waits_for_posts() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    discord.block_sends_to([100, 200]);
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let player_ids = fixture.add_shuffle_pool(10, false);
    let prepared = fixture.prepare_shuffle(player_ids, "glicko", Vec::new());
    let pending_match_id = prepared.pending.pending_match_id;
    let responder = Arc::new(FailingConfirmationResponder {
        started: AtomicBool::new(false),
    });
    let handler = Arc::clone(&fixture.provider.handler);
    let responder_for_task: Arc<dyn InteractionResponder> = responder.clone();
    let mut task = tokio::spawn(async move {
        handler
            .finalize_shuffle(
                shuffle_context(Some(200)),
                responder_for_task,
                publication_snapshot(LobbyKind::Open, None),
                prepared,
            )
            .await
    });

    wait_for_count(&discord.blocked_send_started, 2).await;
    while !responder.started.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut task)
            .await
            .is_err(),
        "the failed confirmation must not detach either public post"
    );

    discord.send_gate.add_permits(2);
    let error = task
        .await
        .expect("publication task joins")
        .expect_err("confirmation failure is propagated");
    assert!(error.to_string().contains("confirmation failed"));
    assert!(discord.unpins().is_empty());
    assert!(discord.thread_edits().is_empty());
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending_match_id)
            .expect("read failed publication")
            .is_some()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_public_send_failures_are_isolated_and_logged() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    discord.fail_sends_to([100, 200]);
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let player_ids = fixture.add_shuffle_pool(10, false);
    let prepared = fixture.prepare_shuffle(player_ids, "glicko", Vec::new());
    let pending_match_id = prepared.pending.pending_match_id;
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .finalize_shuffle(
            shuffle_context(Some(200)),
            responder.clone(),
            publication_snapshot(LobbyKind::LowSkill, None),
            prepared,
        )
        .await
        .expect("public post failures are isolated");

    let responses = responder
        .responses
        .lock()
        .expect("publication confirmation");
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].content, "✅ 🧀 Whine & Cheese teams shuffled!");
    assert!(responses[0].ephemeral);
    drop(responses);
    let persisted = PendingMatchRepository::new(fixture.database.path())
        .pending_match(GUILD, pending_match_id)
        .expect("read publication metadata")
        .expect("pending match remains");
    assert_eq!(persisted.state.shuffle_message_id, None);
    assert_eq!(persisted.state.cmd_shuffle_message_id, None);
    assert_eq!(discord.unpins(), vec![(100, 321)]);
    abort_betting_tasks(&fixture, pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_thread_and_pin_cleanup_finish_before_reset() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    discord.block_sends_to([42]);
    discord.block_unpin.store(true, Ordering::Release);
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let player_ids = fixture.add_shuffle_pool(10, false);
    let prepared = fixture.prepare_shuffle(player_ids, "glicko", Vec::new());
    let pending_match_id = prepared.pending.pending_match_id;
    let handler = Arc::clone(&fixture.provider.handler);
    let mut task = tokio::spawn(async move {
        handler
            .finalize_shuffle(
                shuffle_context(Some(200)),
                Arc::new(RecordingMatchResponder::default()),
                publication_snapshot(LobbyKind::Open, Some(42)),
                prepared,
            )
            .await
    });

    wait_for_count(&discord.blocked_send_started, 1).await;
    while !discord.unpin_started.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut task)
            .await
            .is_err(),
        "lobby reset must wait for both maintenance branches"
    );
    discord.send_gate.add_permits(2);
    discord.unpin_gate.add_permits(1);
    task.await
        .expect("maintenance task joins")
        .expect("maintenance completes");

    let order = discord.order();
    let unpin_done = order
        .iter()
        .position(|entry| entry == "unpin:done")
        .expect("unpin completed");
    let lock_done = order
        .iter()
        .position(|entry| entry == "thread:lock:done")
        .expect("thread lock completed");
    assert!(unpin_done < order.len());
    assert!(lock_done < order.len());
    let persisted = PendingMatchRepository::new(fixture.database.path())
        .pending_match(GUILD, pending_match_id)
        .expect("read thread metadata")
        .expect("pending match remains");
    assert_eq!(persisted.state.thread_shuffle_thread_id, Some(42));
    assert!(persisted.state.thread_shuffle_message_id.is_some());
    abort_betting_tasks(&fixture, pending_match_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_abort_does_not_touch_a_new_or_sibling_lobby() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let aborted = fixture.pending(unix_seconds() + 120);
    let repository = PendingMatchRepository::new(fixture.database.path());
    let sibling = repository
        .create_pending_match(
            GUILD,
            &PendingMatchState {
                lobby_kind: Some("lowskill".to_owned()),
                shuffle_timestamp: Some(unix_seconds()),
                ..PendingMatchState::default()
            },
        )
        .expect("create sibling pending match");

    fixture
        .provider
        .handler
        .finalize_abort_owned(&aborted)
        .await
        .expect("abort selected match");

    assert!(
        repository
            .pending_match(GUILD, aborted.pending_match_id)
            .expect("read aborted match")
            .is_none()
    );
    assert_eq!(
        repository
            .pending_match(GUILD, sibling.pending_match_id)
            .expect("read sibling match")
            .expect("sibling remains")
            .state
            .lobby_kind
            .as_deref(),
        Some("lowskill")
    );
    assert!(discord.unpins().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_abort_pings_everyone_from_the_shuffled_lobby() {
    let fixture = MatchRuntimeFixture::new();
    let pending = PendingMatchRepository::new(fixture.database.path())
        .create_pending_match(
            GUILD,
            &PendingMatchState {
                radiant_team_ids: vec![30, 10],
                dire_team_ids: vec![40, 20],
                excluded_player_ids: vec![50, 10],
                excluded_conditional_player_ids: vec![60, 50],
                shuffle_timestamp: Some(unix_seconds()),
                ..PendingMatchState::default()
            },
        )
        .expect("create full shuffled lobby");
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .finalize_abort(&pending, responder.clone())
        .await
        .expect("abort full lobby");

    let responses = responder.responses.lock().expect("abort response");
    assert_eq!(responses.len(), 1);
    assert_eq!(
        responses[0].content,
        format!(
            "✅ Match aborted (Match #{}). Bets have been refunded.\n<@10> <@20> <@30> <@40> <@50> <@60>",
            pending.pending_match_id
        )
    );
    assert_eq!(
        responses[0].allowed_mentions,
        InteractionAllowedMentions::Users(vec![10, 20, 30, 40, 50, 60])
    );
    assert!(!responses[0].ephemeral);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_abort_refund_failure_keeps_pending_match_retryable() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    Connection::open(fixture.database.path())
        .expect("open refund failure fixture")
        .execute_batch("DROP TABLE bets")
        .expect("break only the refund dependency");
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .finalize_abort(&pending, responder.clone())
        .await
        .expect("refund failure is a friendly response");

    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read retryable pending match")
            .is_some()
    );
    let responses = responder.responses.lock().expect("refund failure response");
    assert_eq!(responses.len(), 1);
    assert_eq!(
        responses[0].content,
        "❌ Match abort failed while refunding bets. Please try again."
    );
    assert!(responses[0].ephemeral);
}

#[tokio::test(start_paused = true)]
async fn test_completed_match_thread_retains_source_lobby_label() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let pending = fixture.pending(unix_seconds() + 120);
    let repository = PendingMatchRepository::new(fixture.database.path());
    let pending = repository
        .mutate_pending_match(GUILD, pending.pending_match_id, |state| {
            state.lobby_kind = Some("lowskill".to_owned());
            state.thread_shuffle_thread_id = Some(42);
        })
        .expect("persist source lobby label")
        .expect("pending match exists")
        .0;
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .handle_record(
            record_context(pending.state.radiant_team_ids[0], GUILD, "radiant", true),
            responder,
        )
        .await
        .expect("record low-skill match");
    for _ in 0..100 {
        if discord
            .sent_messages()
            .iter()
            .any(|(channel_id, _)| *channel_id == 42)
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    tokio::time::advance(std::time::Duration::from_secs(15)).await;
    for _ in 0..100 {
        if !discord.thread_edits().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }

    let thread_message = discord
        .sent_messages()
        .into_iter()
        .find(|(channel_id, _)| *channel_id == 42)
        .expect("thread result message")
        .1;
    assert_eq!(
        thread_message.response.content,
        "🏆 **🧀 Whine & Cheese Match Complete - Radiant Victory!**"
    );
    assert!(discord.thread_edits().contains(&(
        42,
        "✅ 🧀 Whine & Cheese Match Complete - Radiant Won".to_owned(),
        true,
        true,
    )));
}

#[tokio::test(start_paused = true)]
async fn test_record_scopes_finalize_to_recorded_match() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let repository = PendingMatchRepository::new(fixture.database.path());
    let first = fixture.pending(unix_seconds() + 120);
    let second = repository
        .create_pending_match(GUILD, &first.state)
        .expect("create sibling pending match");
    let first = repository
        .mutate_pending_match(GUILD, first.pending_match_id, |state| {
            state.thread_shuffle_thread_id = Some(501);
        })
        .expect("assign first match thread")
        .expect("first pending match exists")
        .0;
    repository
        .mutate_pending_match(GUILD, second.pending_match_id, |state| {
            state.thread_shuffle_thread_id = Some(502);
        })
        .expect("assign sibling match thread")
        .expect("sibling pending match exists");

    fixture
        .provider
        .handler
        .finalize_record(
            first.clone(),
            record_context(first.state.radiant_team_ids[0], GUILD, "radiant", true),
            Arc::new(RecordingMatchResponder::default()),
            "radiant",
            3,
            3,
            0,
            None,
        )
        .await
        .expect("record the selected pending match");
    for _ in 0..100 {
        if discord
            .sent_messages()
            .iter()
            .any(|(channel_id, _)| *channel_id == 501)
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(15)).await;
    for _ in 0..100 {
        if !discord.thread_edits().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }

    assert!(
        repository
            .pending_match(GUILD, first.pending_match_id)
            .expect("read recorded pending match")
            .is_none()
    );
    assert!(
        repository
            .pending_match(GUILD, second.pending_match_id)
            .expect("read sibling pending match")
            .is_some()
    );
    assert_eq!(
        discord.thread_edits(),
        vec![(
            501,
            "✅ 🍽️ All You Can Feed Match Complete - Radiant Won".to_owned(),
            true,
            true,
        )]
    );
    assert!(
        discord
            .sent_messages()
            .iter()
            .all(|(channel_id, _)| *channel_id != 502)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_record_first_match_has_no_direct_first_game_award() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .finalize_record(
            pending.clone(),
            record_context(pending.state.radiant_team_ids[0], GUILD, "radiant", true),
            responder.clone(),
            "radiant",
            3,
            3,
            0,
            None,
        )
        .await
        .expect("record first match");

    let announcement = responder.contents().join("\n");
    assert!(announcement.contains("✅ Match recorded"));
    assert!(announcement.contains("🪙 **JC Changes:**"));
    assert!(announcement.contains("win +20"));
    assert!(announcement.contains("play +2"));
    assert!(!announcement.contains("First game of the night"));
    assert!(
        MatchRepository::new(fixture.database.path())
            .match_id_for_pending_match(GUILD, pending.pending_match_id)
            .expect("read first recorded match")
            .is_some()
    );
}

#[tokio::test(start_paused = true)]
async fn test_record_chunks_long_final_message_before_thread_finalize() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let repository = PendingMatchRepository::new(fixture.database.path());
    let pending = fixture.pending(unix_seconds() + 120);
    let pending = repository
        .mutate_pending_match(GUILD, pending.pending_match_id, |state| {
            state.thread_shuffle_thread_id = Some(503);
        })
        .expect("assign long-message thread")
        .expect("long-message pending match exists")
        .0;
    let players = PlayerRepository::new(fixture.database.path());
    let betting = BettingServiceRepository::new(fixture.database.path());
    for index in 0..80_i64 {
        let discord_id = 93_000 + index;
        let mut player = NewPlayer::new(discord_id, format!("bettor-{index}"), Some(GUILD));
        player.initial_mmr = Some(4_000);
        players.add(&player).expect("insert long-summary bettor");
        Connection::open(fixture.database.path())
            .expect("open long-summary balance")
            .execute(
                "UPDATE players SET jopacoin_balance=500
                 WHERE guild_id=?1 AND discord_id=?2",
                params![GUILD, discord_id],
            )
            .expect("fund long-summary bettor");
        betting
            .place_bet_atomic(PlaceBetRequest {
                guild_id: Some(GUILD),
                pending_match_id: pending.pending_match_id,
                discord_id,
                team: if index % 2 == 0 {
                    BettingTeam::Radiant
                } else {
                    BettingTeam::Dire
                },
                amount: 10,
                bet_time: pending.state.shuffle_timestamp.unwrap_or_default(),
                leverage: 1,
                max_debt: 500,
                is_blind: false,
                odds_at_placement: None,
            })
            .expect("place long-summary bet");
    }
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .finalize_record(
            pending.clone(),
            record_context(pending.state.radiant_team_ids[0], GUILD, "radiant", true),
            responder.clone(),
            "radiant",
            3,
            3,
            0,
            None,
        )
        .await
        .expect("record match with long betting summary");
    let responses = responder
        .responses
        .lock()
        .expect("long-summary responses")
        .clone();
    assert!(responses.len() > 1);
    assert!(
        responses
            .iter()
            .all(|response| response.content.chars().count() <= 2_000)
    );
    assert!(
        responses
            .iter()
            .any(|response| response.content.contains("✅ Match recorded"))
    );
    for _ in 0..100 {
        if discord
            .sent_messages()
            .iter()
            .any(|(channel_id, _)| *channel_id == 503)
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(15)).await;
    for _ in 0..100 {
        if !discord.thread_edits().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(discord.thread_edits().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn test_record_finalizes_thread_even_if_final_followup_send_fails() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let pending = fixture.pending(unix_seconds() + 120);
    let pending = PendingMatchRepository::new(fixture.database.path())
        .mutate_pending_match(GUILD, pending.pending_match_id, |state| {
            state.thread_shuffle_thread_id = Some(504);
        })
        .expect("assign failed-followup thread")
        .expect("failed-followup pending match exists")
        .0;
    let responder = Arc::new(FailingConfirmationResponder {
        started: AtomicBool::new(false),
    });

    let error = fixture
        .provider
        .handler
        .finalize_record(
            pending.clone(),
            record_context(pending.state.radiant_team_ids[0], GUILD, "radiant", true),
            responder.clone(),
            "radiant",
            3,
            3,
            0,
            None,
        )
        .await
        .expect_err("final followup fails");
    assert!(error.to_string().contains("confirmation failed"));
    assert!(responder.started.load(Ordering::Acquire));
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read completed pending match")
            .is_none()
    );
    for _ in 0..100 {
        if discord
            .sent_messages()
            .iter()
            .any(|(channel_id, _)| *channel_id == 504)
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(15)).await;
    for _ in 0..100 {
        if !discord.thread_edits().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        discord.thread_edits(),
        vec![(
            504,
            "✅ 🍽️ All You Can Feed Match Complete - Radiant Won".to_owned(),
            true,
            true,
        )]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_record_captures_pet_activity_before_final_followup_send() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let pet_owner = pending.state.radiant_team_ids[0];
    let now = unix_seconds();
    let connection = Connection::open(fixture.database.path()).expect("open pet activity fixture");
    connection
        .execute(
            "INSERT INTO pets (
                 discord_id,guild_id,name,species,adopted_at,hatched_at,
                 adopt_fee,last_fed_at,hunger_at_last_fed,
                 evolution_started_at,evolution_due_at
             ) VALUES (?1,?2,'Blep','camel',?3,?3,0,?3,100,?3,?4)",
            params![pet_owner, GUILD, now - 60, now + 604_800],
        )
        .expect("seed upbringing pet");
    drop(connection);
    let responder = Arc::new(FailingConfirmationResponder {
        started: AtomicBool::new(false),
    });

    fixture
        .provider
        .handler
        .finalize_record(
            pending.clone(),
            record_context(pet_owner, GUILD, "radiant", true),
            responder,
            "radiant",
            3,
            3,
            0,
            None,
        )
        .await
        .expect_err("final announcement fails after pet activity");

    let activity = Connection::open(fixture.database.path())
        .expect("inspect pet activity")
        .query_row(
            "SELECT score,first_source_key
             FROM pet_evolution_daily
             WHERE guild_id=?1 AND instinct='competition'",
            [GUILD],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("read match pet activity");
    let match_id = MatchRepository::new(fixture.database.path())
        .get_most_recent_match(Some(GUILD))
        .expect("read pet activity match")
        .expect("pet activity match exists")
        .match_id;
    assert_eq!(activity, (2, format!("match:{match_id}")));
}

#[tokio::test]
async fn test_finalize_archives_recorded_matchs_thread() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    MatchHandler::finalize_record_thread(
        discord.clone(),
        505,
        "🍽️ All You Can Feed".to_owned(),
        "Radiant".to_owned(),
        Duration::ZERO,
    )
    .await;

    let sent = discord.sent_messages();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, 505);
    assert_eq!(
        sent[0].1.response.content,
        "🏆 **🍽️ All You Can Feed Match Complete - Radiant Victory!**"
    );
    assert_eq!(
        discord.thread_edits(),
        vec![(
            505,
            "✅ 🍽️ All You Can Feed Match Complete - Radiant Won".to_owned(),
            true,
            true,
        )]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_aborted_match_thread_retains_source_lobby_label() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let pending = PendingMatchRepository::new(fixture.database.path())
        .create_pending_match(
            GUILD,
            &PendingMatchState {
                lobby_kind: Some("lowskill".to_owned()),
                thread_shuffle_thread_id: Some(42),
                shuffle_timestamp: Some(unix_seconds()),
                ..PendingMatchState::default()
            },
        )
        .expect("create low-skill pending match");

    fixture
        .provider
        .handler
        .finalize_abort_owned(&pending)
        .await
        .expect("abort low-skill match");

    let thread_message = discord
        .sent_messages()
        .into_iter()
        .find(|(channel_id, _)| *channel_id == 42)
        .expect("abort thread message")
        .1;
    assert_eq!(
        thread_message.response.content,
        "🚫 **🧀 Whine & Cheese Match Aborted** - All bets have been refunded."
    );
    assert_eq!(
        discord.thread_edits(),
        vec![(
            42,
            "🚫 🧀 Whine & Cheese Match Aborted".to_owned(),
            true,
            true,
        )]
    );
}

async fn assert_thread_rename_overlaps_ordered_posts_and_lock_waits_for_both() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    discord.block_rename.store(true, Ordering::Release);
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let pending = PendingMatchRepository::new(fixture.database.path())
        .create_pending_match(
            GUILD,
            &PendingMatchState {
                radiant_team_ids: vec![10, -1],
                dire_team_ids: vec![20],
                ..PendingMatchState::default()
            },
        )
        .expect("create thread publication match");
    let handler = Arc::clone(&fixture.provider.handler);
    let pending_for_task = pending.clone();
    let mut task = tokio::spawn(async move {
        handler
            .publish_shuffle_thread(
                &publication_snapshot(LobbyKind::Open, Some(42)),
                &pending_for_task,
                InteractionEmbed::titled("Teams"),
            )
            .await
    });

    while !discord.rename_started.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    wait_for_sent(&discord, 2).await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut task)
            .await
            .is_err(),
        "thread lock must await the blocked rename"
    );
    assert!(
        !discord
            .thread_edits()
            .iter()
            .any(|(_, _, archived, locked)| !archived && *locked)
    );

    discord.rename_gate.add_permits(1);
    task.await
        .expect("thread publication task joins")
        .expect("thread publication succeeds");
    let order = discord.order();
    let embed = order
        .iter()
        .position(|entry| entry == "send:42:embed:done")
        .expect("embed sent");
    let ping = order
        .iter()
        .position(|entry| entry == "send:42:content:done")
        .expect("ping sent");
    let rename_done = order
        .iter()
        .position(|entry| entry == "thread:rename:done")
        .expect("rename completed");
    let lock = order
        .iter()
        .position(|entry| entry == "thread:lock:started")
        .expect("thread locked");
    assert!(embed < ping);
    assert!(ping < lock);
    assert!(rename_done < lock);
    let ping_message = discord
        .sent_messages()
        .into_iter()
        .find(|(_, message)| message.response.content.contains("starting positions"))
        .expect("participant ping")
        .1;
    assert_eq!(
        ping_message.response.content,
        "<@10> <@20>\nPlayers, take your starting positions"
    );
    assert_eq!(
        ping_message.allowed_mentions,
        crate::discord_transport::DiscordAllowedMentions::Users(BTreeSet::from([10, 20]))
    );
    let persisted = PendingMatchRepository::new(fixture.database.path())
        .pending_match(GUILD, pending.pending_match_id)
        .expect("read thread publication metadata")
        .expect("pending thread match remains");
    assert_eq!(persisted.state.thread_shuffle_thread_id, Some(42));
    assert!(persisted.state.thread_shuffle_message_id.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_thread_rename_overlaps_ordered_posts_and_lock_waits_for_both() {
    assert_thread_rename_overlaps_ordered_posts_and_lock_waits_for_both().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_draft_thread_rename_overlaps_ordered_posts_and_lock_waits() {
    // Draft and shuffle publication intentionally share this production
    // thread finalizer. Keep a distinct contract ID so parity cannot mask a
    // future Draft-only call-site regression with the shuffle mapping.
    assert_thread_rename_overlaps_ordered_posts_and_lock_waits_for_both().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_abort_announcement_filters_fake_negative_player_ids() {
    let fixture = MatchRuntimeFixture::new();
    let pending = PendingMatchRepository::new(fixture.database.path())
        .create_pending_match(
            GUILD,
            &PendingMatchState {
                radiant_team_ids: vec![1, 2, -1],
                dire_team_ids: vec![3, -2],
                excluded_player_ids: vec![4],
                excluded_conditional_player_ids: vec![-3],
                shuffle_timestamp: Some(unix_seconds()),
                ..PendingMatchState::default()
            },
        )
        .expect("create fake-filled pending match");
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .finalize_abort(&pending, responder.clone())
        .await
        .expect("abort fake-filled lobby");

    let responses = responder.responses.lock().expect("fake abort response");
    assert_eq!(responses.len(), 1);
    assert!(!responses[0].content.contains("<@-"));
    assert_eq!(
        responses[0].allowed_mentions,
        InteractionAllowedMentions::Users(vec![1, 2, 3, 4])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_concurrent_abort_finalizations_only_announce_once() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    discord.block_sends_to([42]);
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let pending = PendingMatchRepository::new(fixture.database.path())
        .create_pending_match(
            GUILD,
            &PendingMatchState {
                radiant_team_ids: vec![1, 2],
                dire_team_ids: vec![3, 4],
                thread_shuffle_thread_id: Some(42),
                shuffle_timestamp: Some(unix_seconds()),
                ..PendingMatchState::default()
            },
        )
        .expect("create concurrently aborted match");
    let responder = Arc::new(RecordingMatchResponder::default());
    let handler = Arc::clone(&fixture.provider.handler);
    let pending_for_task = pending.clone();
    let responder_for_task: Arc<dyn InteractionResponder> = responder.clone();
    let first = tokio::spawn(async move {
        handler
            .finalize_abort(&pending_for_task, responder_for_task)
            .await
    });
    wait_for_count(&discord.blocked_send_started, 1).await;

    fixture
        .provider
        .handler
        .finalize_abort(&pending, responder.clone())
        .await
        .expect("second abort is rejected without replay");
    discord.send_gate.add_permits(1);
    first
        .await
        .expect("first abort task joins")
        .expect("first abort succeeds");

    let responses = responder
        .responses
        .lock()
        .expect("concurrent abort responses");
    assert_eq!(
        responses
            .iter()
            .filter(|response| !response.ephemeral)
            .count(),
        1
    );
    assert_eq!(
        responses
            .iter()
            .filter(|response| response.ephemeral)
            .count(),
        1
    );
    assert_eq!(
        discord
            .sent_messages()
            .iter()
            .filter(|(channel_id, _)| *channel_id == 42)
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_finalize_abort_with_cleared_state_does_not_reannounce() {
    let fixture = MatchRuntimeFixture::new();
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .handle_record(record_context(1, GUILD, "abort", true), responder.clone())
        .await
        .expect("cleared pending state is a friendly response");

    let responses = responder.responses.lock().expect("cleared abort response");
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].content, "❌ No pending match to record.");
    assert!(responses[0].ephemeral);
}

#[tokio::test]
async fn discovered_recorded_match_is_published_once_to_the_persisted_origin_channel() {
    use cama_app::match_discovery::{
        DiscoveryResult, DiscoveryStatus, InternalMatchId, ValveMatchId,
    };

    let response = InteractionResponse::message("📊 Match #91001 auto-enriched (100% confidence)")
        .embed(InteractionEmbed::titled("Enriched match"));
    let discovery: Arc<dyn RecordedMatchDiscovery> = Arc::new(StaticRecordedDiscovery {
        outcome: RecordedMatchDiscoveryOutcome::Discovered {
            result: DiscoveryResult {
                match_id: InternalMatchId(MATCH_ID),
                status: DiscoveryStatus::Discovered,
                valve_match_id: Some(ValveMatchId(92_001)),
                confidence: Some(1.0),
                player_count: 10,
                total_players: 10,
                players_with_steam_id: 10,
                validation_error: None,
            },
            response: response.clone(),
        },
    });
    let discord = Arc::new(PublicationDiscord::default());
    MatchHandler::run_recorded_match_discovery(
        discovery,
        discord.clone(),
        Some(77_001),
        GUILD,
        MATCH_ID,
    )
    .await;

    let sent = discord.sent.lock().expect("publication capture");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, 77_001);
    assert_eq!(sent[0].1.response, response);
    assert_eq!(
        sent[0].1.allowed_mentions,
        crate::discord_transport::DiscordAllowedMentions::None
    );
}

#[test]
fn generated_match_reward_retry_recovers_base_and_blood_pact_without_second_payment() {
    let (database, control) = reward_fixture();
    let players = [WINNER];
    let batch = GeneratedRewardBatch {
        guild_id: GUILD,
        player_ids: &players,
        gross: 100,
        apply_bankruptcy_penalty: true,
        apply_vanity_tax: true,
        source: "match_streaming_bonus",
        related_type: "match",
        related_id: MATCH_ID,
        reason: "match streaming bonus",
        event_nonce_tag: 6,
    };
    let first = control
        .award_generated_batch(batch)
        .expect("first generated reward");
    assert!(first[&WINNER].receipt.applied);
    let first_balance = Connection::open(database.path())
        .expect("read first generated balance")
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE guild_id=?1 AND discord_id=?2",
            params![GUILD, WINNER],
            |row| row.get::<_, i64>(0),
        )
        .expect("first generated player balance");

    let retry = control
        .award_generated_batch(batch)
        .expect("recover generated reward");
    assert!(!retry[&WINNER].receipt.applied);
    assert_eq!(
        retry[&WINNER].receipt.balance_delta,
        first[&WINNER].receipt.balance_delta
    );
    assert_eq!(
        retry[&WINNER].blood_pact_skim,
        first[&WINNER].blood_pact_skim
    );
    let connection = Connection::open(database.path()).expect("inspect generated retry");
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE guild_id=?1 AND discord_id=?2",
                params![GUILD, WINNER],
                |row| row.get::<_, i64>(0),
            )
            .expect("recovered generated player balance"),
        first_balance
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM hostile_loss_events
                 WHERE event_key=?1",
                [format!(
                    "match-reward-blood-pact:match:{MATCH_ID}:match_streaming_bonus:6:{WINNER}"
                )],
                |row| row.get::<_, i64>(0),
            )
            .expect("generated pact event count"),
        1
    );
}

#[test]
fn prepared_shuffle_runs_blind_investment_and_spectator_batches_on_migrated_sqlite() {
    let fixture = MatchRuntimeFixture::new();
    let player_ids = fixture.add_shuffle_pool(10, false);
    let spectator_id = 85_500;
    let players = PlayerRepository::new(fixture.database.path());
    players
        .add(&NewPlayer::new(
            spectator_id,
            "automatic-spectator".to_owned(),
            Some(GUILD),
        ))
        .expect("insert automatic spectator");
    Connection::open(fixture.database.path())
        .expect("open automatic betting fixture")
        .execute(
            "UPDATE players SET jopacoin_balance=51
             WHERE guild_id=?1 AND discord_id=?2",
            params![GUILD, player_ids[0]],
        )
        .expect("fund investment player");
    Connection::open(fixture.database.path())
        .expect("open spectator balance fixture")
        .execute(
            "UPDATE players SET jopacoin_balance=500
             WHERE guild_id=?1 AND discord_id=?2",
            params![GUILD, spectator_id],
        )
        .expect("fund spectator");
    cama_db::autobet_investments::AutobetInvestmentRepository::new(fixture.database.path())
        .set(Some(GUILD), player_ids[0], player_ids[0], "long", 10)
        .expect("configure long investment");

    let prepared = fixture.prepare_shuffle(player_ids.clone(), "glicko", Vec::new());
    let summary = prepared
        .pending
        .state
        .blind_bets_result
        .expect("automatic-bet summary persisted");
    assert!(summary["created"].as_i64().unwrap_or_default() > 0);
    assert!(
        summary["investment_bets"]["created"]
            .as_i64()
            .unwrap_or_default()
            > 0
    );
    let investment_summary = summary["investment_bets"]["bets"]
        .as_array()
        .expect("investment summary bets");
    assert_eq!(investment_summary.len(), 1);
    assert_eq!(investment_summary[0]["investor_id"], json!(player_ids[0]));
    assert_eq!(investment_summary[0]["target_id"], json!(player_ids[0]));
    assert_eq!(investment_summary[0]["direction"], json!("long"));
    assert_eq!(investment_summary[0]["percentage"], json!(10));
    assert!(
        summary["spectator_bets"]["created"]
            .as_i64()
            .unwrap_or_default()
            > 0
    );
    let connection = Connection::open(fixture.database.path()).expect("inspect automatic bets");
    assert!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM bets
                 WHERE guild_id=?1 AND pending_match_id=?2
                   AND investment_target_id=?3 AND investment_direction='long'",
                params![GUILD, prepared.pending.pending_match_id, player_ids[0]],
                |row| row.get::<_, i64>(0),
            )
            .expect("count configured investment")
            > 0
    );
    assert!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM bets
                 WHERE guild_id=?1 AND pending_match_id=?2 AND discord_id=?3",
                params![GUILD, prepared.pending.pending_match_id, spectator_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("count spectator investment")
            > 0
    );
}
