use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicUsize;
use std::thread;

use cama_app::opendota_http::OpenDotaHttpConfig;
use cama_db::gambling_stats_repository::{
    AutoBetGroupStats, AutoBetPerformance, PlayerAutoBetStats,
};
use cama_db::predictions_repository::{ContractSide, NewLevel, PredictionRepository};
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::discord_transport::GuildPlayerNameDirectory;
use crate::registration::{
    InteractionMessageDelivery, InteractionOption, InteractionResponseError, InteractionValue,
    Registry, RegistryBuilder,
};
use crate::test_support::initialize_test_database as initialize_or_migrate;

#[derive(Debug, Default)]
struct CapturedResponses {
    immediate: Vec<InteractionResponse>,
    deferred: Vec<bool>,
    followups: Vec<InteractionResponse>,
    updates: Vec<InteractionResponse>,
    edits: Vec<InteractionResponse>,
    deleted: Vec<InteractionMessageReceipt>,
}

#[derive(Debug, Default)]
struct CapturingResponder {
    captured: StdMutex<CapturedResponses>,
}

struct FixedProfileNames;

impl GuildPlayerNameResolver for FixedProfileNames {
    fn names_for_guild(
        &self,
        _guild_id: i64,
        player_ids: &[i64],
    ) -> Result<GuildPlayerNameDirectory, String> {
        Ok(GuildPlayerNameDirectory::new(Some(
            player_ids
                .iter()
                .filter_map(|player_id| {
                    u64::try_from(*player_id)
                        .ok()
                        .map(|player_id| (player_id, "Discord Display".to_owned()))
                })
                .collect(),
        )))
    }
}

#[async_trait]
impl InteractionResponder for CapturingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture lock")
            .immediate
            .push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture lock")
            .deferred
            .push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture lock")
            .followups
            .push(response);
        Ok(())
    }

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        self.followup(response).await?;
        Ok(Some(InteractionMessageReceipt {
            message_id: 77,
            channel_id: 9,
            delivery: InteractionMessageDelivery::InteractionFollowup,
        }))
    }

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture lock")
            .updates
            .push(response);
        Ok(())
    }

    async fn edit_original(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture lock")
            .edits
            .push(response);
        Ok(())
    }

    async fn delete_message(
        &self,
        receipt: InteractionMessageReceipt,
    ) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture lock")
            .deleted
            .push(receipt);
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct RouteServer {
    base_url: String,
    requests: Arc<StdMutex<Vec<String>>>,
}

impl RouteServer {
    fn start(expected_requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback OpenDota server");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("loopback address")
        );
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        thread::spawn(move || {
            for _ in 0..expected_requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let Some(request) = read_request(&mut stream) else {
                    return;
                };
                captured
                    .lock()
                    .expect("request capture lock")
                    .push(request.clone());
                let body = route_body(&request);
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(wire.as_bytes());
            }
        });
        Self { base_url, requests }
    }

    fn wait_for_requests(&self, expected: usize) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let requests = self.requests.lock().expect("request capture lock").clone();
            if requests.len() >= expected || Instant::now() >= deadline {
                return requests;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<String> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1_024];
    while bytes.len() < 16 * 1_024 {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(bytes)
        .ok()?
        .lines()
        .next()
        .map(ToOwned::to_owned)
}

fn route_body(request: &str) -> &'static str {
    if request.starts_with("GET /players/12345/wl ") {
        r#"{"win":30,"lose":20}"#
    } else if request.starts_with("GET /players/12345/totals ") {
        r#"[{"field":"kills","n":10,"sum":50},{"field":"deaths","n":10,"sum":30},{"field":"assists","n":10,"sum":80},{"field":"gold_per_min","n":10,"sum":5000},{"field":"xp_per_min","n":10,"sum":6000}]"#
    } else if request.starts_with("GET /players/12345/heroes ") {
        r#"[{"hero_id":1,"games":10,"win":6}]"#
    } else if request.starts_with("GET /players/12345/matches?") {
        r#"[{"match_id":9001,"hero_id":1,"lane_role":2,"player_slot":0,"radiant_win":true,"kills":5,"deaths":3,"assists":8,"duration":2400,"start_time":1800000000}]"#
    } else if request.starts_with("GET /players/12345 ") {
        r#"{"profile":{"personaname":"Open Player"},"rank_tier":75,"mmr_estimate":{"estimate":5000}}"#
    } else {
        r#"{"error":"unexpected test route"}"#
    }
}

fn migrated_player_fixture() -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary profile database");
    initialize_or_migrate(database.path()).expect("initialize Python-compatible schema");
    let connection = Connection::open(database.path()).expect("open profile fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match production SQLite policy");
    connection
        .execute(
            "INSERT INTO players (
                 discord_id,guild_id,discord_username,current_mmr,initial_mmr,
                 wins,losses,preferred_roles,main_role,glicko_rating,glicko_rd,
                 glicko_volatility,os_mu,os_sigma,jopacoin_balance,steam_id,
                 preferred_region
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
                100_i64,
                42_i64,
                "Database Name",
                4_500_i64,
                4_000_i64,
                12_i64,
                8_i64,
                r#"["1","5"]"#,
                "1",
                1_750.0_f64,
                70.0_f64,
                0.06_f64,
                48.0_f64,
                5.0_f64,
                321_i64,
                12_345_i64,
                "USW",
            ],
        )
        .expect("insert profile player");
    database
}

fn loopback_services(server: &RouteServer) -> Arc<OpenDotaRuntimeServices> {
    let mut config = OpenDotaHttpConfig::new(&server.base_url, None);
    config.request_timeout = Duration::from_secs(2);
    config.request_deadline = Duration::from_secs(5);
    config.retry_delays = vec![Duration::ZERO];
    config.requests_per_minute = 1_200;
    config.hero_names.insert(1, "Anti-Mage".to_owned());
    Arc::new(OpenDotaRuntimeServices::with_config(config).expect("loopback OpenDota services"))
}

fn offline_services() -> Arc<OpenDotaRuntimeServices> {
    let mut config = OpenDotaHttpConfig::new("http://127.0.0.1:9", None);
    config.request_timeout = Duration::from_millis(10);
    config.request_deadline = Duration::from_millis(50);
    config.retry_delays = vec![Duration::ZERO];
    Arc::new(OpenDotaRuntimeServices::with_config(config).expect("offline OpenDota services"))
}

#[allow(clippy::too_many_arguments)]
fn insert_profile_pairing(
    database: &NamedTempFile,
    peer_id: i64,
    games_together: i64,
    wins_together: i64,
    games_against: i64,
    player_wins_against: i64,
) {
    let connection = Connection::open(database.path()).expect("open profile pairing fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("allow focused aggregate fixture");
    connection
        .execute(
            "INSERT INTO player_pairings (
                 guild_id, player1_id, player2_id,
                 games_together, wins_together,
                 games_against, player1_wins_against
             ) VALUES (42, 100, ?1, ?2, ?3, ?4, ?5)",
            params![
                peer_id,
                games_together,
                wins_together,
                games_against,
                player_wins_against
            ],
        )
        .expect("insert profile pairing aggregate");
}

fn teammates_page(database: &NamedTempFile, target_id: i64, target_name: &str) -> ProfilePage {
    ProfileDataSources::new(database.path(), offline_services())
        .teammates(target_id, Some(42), target_name)
        .expect("render teammates profile page")
}

fn teammates_field<'a>(page: &'a ProfilePage, name: &str) -> &'a ProfileField {
    page.fields
        .iter()
        .find(|field| field.name.contains(name))
        .unwrap_or_else(|| panic!("missing teammates field {name:?}"))
}

fn insert_rating_chart_fixture(database: &NamedTempFile) {
    let connection = Connection::open(database.path()).expect("open profile fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable legacy foreign-key mismatch for fixture row");
    connection
        .execute(
            "INSERT INTO rating_history (
                 guild_id, discord_id, rating, won, match_id, timestamp,
                 os_mu_before, os_mu_after, os_sigma_before, os_sigma_after
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                42_i64,
                100_i64,
                1_500.0_f64,
                true,
                2_i64,
                "2026-01-02T00:00:00",
                25.0_f64,
                26.0_f64,
                8.0_f64,
                7.5_f64,
            ],
        )
        .expect("insert Glicko history row");
    connection
        .execute(
            "INSERT INTO rating_history (
                 guild_id, discord_id, rating, won, match_id, timestamp,
                 os_mu_before, os_mu_after, os_sigma_before, os_sigma_after
             ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                42_i64,
                100_i64,
                false,
                1_i64,
                "2026-01-01T00:00:00",
                24.0_f64,
                25.0_f64,
                8.5_f64,
                8.0_f64,
            ],
        )
        .expect("insert OpenSkill-only history row");
}

fn insert_gambling_chart_fixture(database: &NamedTempFile) {
    insert_gambling_chart_fixture_for(
        database,
        &[
            (10_i64, 1_i64, 1_700_000_000_i64, Some(20_i64)),
            (11_i64, 2_i64, 1_700_000_100_i64, None),
        ],
    );
}

fn insert_gambling_chart_fixture_for(
    database: &NamedTempFile,
    events: &[(i64, i64, i64, Option<i64>)],
) {
    let connection = Connection::open(database.path()).expect("open profile fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable legacy foreign-key mismatch for fixture row");
    for &(match_id, winning_team, bet_time, payout) in events {
        connection
            .execute(
                "INSERT INTO matches (
                     match_id, guild_id, team1_players, team2_players, winning_team
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![match_id, 42_i64, "[100]", "[200]", winning_team],
            )
            .expect("insert gambling match");
        connection
            .execute(
                "INSERT INTO match_participants (
                     match_id, discord_id, guild_id, team_number, won, side
                 ) VALUES (?1, ?2, ?3, 1, ?4, 'radiant')",
                params![match_id, 100_i64, 42_i64, i64::from(winning_team == 1)],
            )
            .expect("insert gambling participant");
        connection
            .execute(
                "INSERT INTO bets (
                     guild_id, match_id, discord_id, team_bet_on, amount,
                     bet_time, leverage, payout
                 ) VALUES (?1, ?2, ?3, 'radiant', ?4, ?5, 1, ?6)",
                params![42_i64, match_id, 100_i64, 10_i64, bet_time, payout],
            )
            .expect("insert gambling bet");
    }
}

fn insert_economy_chart_fixture(database: &NamedTempFile) {
    let connection = Connection::open(database.path()).expect("open profile fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable legacy foreign-key mismatch for fixture row");
    for (match_id, winning_team, match_date) in [
        (20_i64, 1_i64, "2026-01-01 00:00:00"),
        (21_i64, 2_i64, "2026-01-02 00:00:00"),
    ] {
        connection
            .execute(
                "INSERT INTO matches (
                     match_id, guild_id, team1_players, team2_players, winning_team, match_date
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![match_id, 42_i64, "[100]", "[200]", winning_team, match_date,],
            )
            .expect("insert economy match");
        connection
            .execute(
                "INSERT INTO match_participants (
                     match_id, discord_id, guild_id, team_number, won, side
                 ) VALUES (?1, ?2, ?3, 1, ?4, 'radiant')",
                params![match_id, 100_i64, 42_i64, i64::from(winning_team == 1)],
            )
            .expect("insert economy participant");
    }
}

#[test]
fn balance_history_snapshot_reads_the_production_adapter_from_an_existing_schema() {
    let database = migrated_player_fixture();
    insert_economy_chart_fixture(&database);

    let snapshot = read_balance_history_snapshot(database.path(), 100, 42)
        .expect("production balance-history snapshot");

    assert_eq!(snapshot.event_count, 2);
    assert_eq!(snapshot.source_event_counts.get("bonus"), Some(&2));
    assert_eq!(snapshot.source_totals.get("bonus"), Some(&15));
    assert_eq!(snapshot.source_event_counts.values().sum::<usize>(), 2);
}

fn insert_heroes_chart_fixture(database: &NamedTempFile) {
    let connection = Connection::open(database.path()).expect("open profile fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable legacy foreign-key mismatch for fixture row");
    for (match_id, hero_id, won, match_date) in [
        (30_i64, 1_i64, true, "2026-01-03 00:00:00"),
        (31_i64, 1_i64, true, "2026-01-04 00:00:00"),
        (32_i64, 2_i64, false, "2026-01-05 00:00:00"),
    ] {
        connection
            .execute(
                "INSERT INTO matches (
                     match_id, guild_id, team1_players, team2_players, winning_team, match_date
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    match_id,
                    42_i64,
                    "[100]",
                    "[200]",
                    i64::from(won),
                    match_date,
                ],
            )
            .expect("insert Heroes match");
        connection
            .execute(
                "INSERT INTO match_participants (
                     match_id, discord_id, guild_id, team_number, won, side,
                     hero_id, kills, deaths, assists, gpm
                 ) VALUES (?1, ?2, ?3, 1, ?4, 'radiant', ?5, 8, 2, 12, 500)",
                params![match_id, 100_i64, 42_i64, i64::from(won), hero_id],
            )
            .expect("insert Heroes participant");
    }
}

fn profile_page(database: &NamedTempFile, tab: ProfileTab) -> ProfilePage {
    ProfileDataSources::new(database.path(), offline_services())
        .build_page(tab, 100, Some(42), "Player")
        .expect("build profile page")
}

fn prediction_page(database: &NamedTempFile) -> ProfilePage {
    profile_page(database, ProfileTab::Predictions)
}

fn insert_profile_prediction(
    database: &NamedTempFile,
    question: &str,
) -> (PredictionRepository, i64) {
    let repository = PredictionRepository::new(database.path());
    let prediction_id = repository
        .create_orderbook_prediction(
            Some(42),
            100,
            question,
            50,
            None,
            &[NewLevel::ask(50, 100), NewLevel::bid(50, 100)],
        )
        .expect("create profile prediction");
    (repository, prediction_id)
}

fn sample_auto_bet_performance(target_count: usize) -> AutoBetPerformance {
    let targets = (0..target_count)
        .map(|index| PlayerAutoBetStats {
            target_id: 9_000_000_000_000_000_000_i64 + i64::try_from(index).unwrap(),
            directions: if index % 2 == 0 {
                vec!["long".to_owned()]
            } else {
                vec!["short".to_owned()]
            },
            bet_count: 12 + index,
            wins: 7 + index / 2,
            total_wagered: 12_345 + i64::try_from(index).unwrap(),
            net_pnl: if index % 3 == 0 {
                -(2_345 + i64::try_from(index).unwrap())
            } else {
                2_345 + i64::try_from(index).unwrap()
            },
        })
        .collect();
    AutoBetPerformance {
        total: AutoBetGroupStats {
            bet_count: 180,
            wins: 101,
            total_wagered: 987_654,
            net_pnl: 12_345,
        },
        generic: AutoBetGroupStats {
            bet_count: 40,
            wins: 19,
            total_wagered: 123_456,
            net_pnl: -7_890,
        },
        targets,
        arbitrage: AutoBetGroupStats {
            bet_count: 13,
            wins: 8,
            total_wagered: 54_321,
            net_pnl: 4_567,
        },
    }
}

#[test]
fn rating_tab_renders_openskill_only_history_as_a_discord_attachment() {
    let database = migrated_player_fixture();
    insert_rating_chart_fixture(&database);

    let sources = ProfileDataSources::new(database.path(), offline_services());
    let page = sources
        .build_page(ProfileTab::Rating, 100, Some(42), "Player")
        .expect("build rating page");
    assert_eq!(
        page.image_url.as_deref(),
        Some("attachment://rating_chart.png")
    );
    assert_eq!(page.attachments.len(), 1);
    assert_eq!(page.attachments[0].filename, "rating_chart.png");
    assert!(page.attachments[0].bytes.starts_with(b"\x89PNG\r\n\x1a\n"));

    let response = profile_response(page, 1, ProfileTab::Rating);
    assert_eq!(response.attachments.len(), 1);
    assert_eq!(response.attachments[0].filename, "rating_chart.png");
    assert_eq!(
        response.embeds[0].image_url.as_deref(),
        Some("attachment://rating_chart.png")
    );
}

#[test]
fn profile_caps_legacy_glicko_rd_at_shared_limit() {
    let database = migrated_player_fixture();
    Connection::open(database.path())
        .expect("open profile fixture")
        .execute(
            "UPDATE players SET glicko_rd = 350.0 WHERE discord_id = 100 AND guild_id = 42",
            [],
        )
        .expect("seed legacy over-cap RD");

    let overview = profile_page(&database, ProfileTab::Overview);
    let overview_rating = overview
        .fields
        .iter()
        .find(|field| field.name == "Rating")
        .expect("overview rating field");
    assert!(overview_rating.value.contains("(0% certain)"));

    let rating = profile_page(&database, ProfileTab::Rating);
    let glicko = rating
        .fields
        .iter()
        .find(|field| field.name == "Glicko")
        .expect("Glicko rating field");
    assert!(glicko.value.contains("**RD:** 250"));
    assert!(!glicko.value.contains("350"));
}

fn registry_for(provider: &ProfileRegistrationProvider) -> Registry {
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register profile");
    builder.build()
}

fn command_request(user_id: u64, options: Vec<InteractionOption>) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "profile".to_owned(),
        user_id,
        user_display_name: "Discord Display".to_owned(),
        guild_id: Some(42),
        channel_id: Some(9),
        member_permissions: None,
        options,
    }
}

fn component_request(user_id: u64, tab: &str) -> InteractionRequest {
    InteractionRequest::Component {
        interaction_id: user_id + 100,
        custom_id: format!("profile:1:{tab}"),
        user_id,
        user_display_name: format!("Profile User {user_id}"),
        guild_id: Some(42),
        channel_id: Some(9),
        member_permissions: None,
        values: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rating_component_uploads_chart_on_original_edit() {
    let database = migrated_player_fixture();
    insert_rating_chart_fixture(&database);
    let provider = ProfileRegistrationProvider::new(database.path(), offline_services());
    let registry = registry_for(&provider);
    let responder = Arc::new(CapturingResponder::default());

    registry
        .command_handler("profile")
        .expect("profile handler")
        .handle(command_request(100, Vec::new()), responder.clone())
        .await
        .expect("profile command");
    registry
        .component_handler("profile:1:rating")
        .expect("profile rating component handler")
        .handle(component_request(100, "rating"), responder.clone())
        .await
        .expect("profile rating component");

    let captured = responder.captured.lock().expect("response capture lock");
    assert_eq!(captured.followups.len(), 1);
    let response = captured
        .edits
        .iter()
        .find(|response| response.embeds[0].title.as_deref() == Some("Profile: 100 > Rating"))
        .expect("rating edit");
    assert_eq!(response.attachments.len(), 1);
    assert_eq!(response.attachments[0].filename, "rating_chart.png");
    assert!(
        response.attachments[0]
            .bytes
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
    assert_eq!(
        response.embeds[0].image_url.as_deref(),
        Some("attachment://rating_chart.png")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gambling_component_uploads_chart_on_original_edit() {
    let database = migrated_player_fixture();
    insert_gambling_chart_fixture(&database);
    let provider = ProfileRegistrationProvider::new(database.path(), offline_services());
    let registry = registry_for(&provider);
    let responder = Arc::new(CapturingResponder::default());

    registry
        .command_handler("profile")
        .expect("profile handler")
        .handle(command_request(100, Vec::new()), responder.clone())
        .await
        .expect("profile command");
    registry
        .component_handler("profile:1:gambling")
        .expect("profile gambling component handler")
        .handle(component_request(100, "gambling"), responder.clone())
        .await
        .expect("profile gambling component");

    let captured = responder.captured.lock().expect("response capture lock");
    let response = captured
        .edits
        .iter()
        .find(|response| response.embeds[0].title.as_deref() == Some("Profile: 100 > Gambling"))
        .expect("gambling edit");
    assert_eq!(response.attachments.len(), 1);
    assert_eq!(response.attachments[0].filename, "gamba_chart.png");
    assert!(
        response.attachments[0]
            .bytes
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
    assert_eq!(
        response.embeds[0].image_url.as_deref(),
        Some("attachment://gamba_chart.png")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn economy_component_uploads_chart_on_original_edit() {
    let database = migrated_player_fixture();
    insert_economy_chart_fixture(&database);
    let sources = ProfileDataSources::new(database.path(), offline_services());
    let history = sources.balance_history.get_balance_event_series(100, 42);
    assert_eq!(history.series.len(), 2);
    assert_eq!(
        history
            .series
            .iter()
            .map(|point| point.event.delta)
            .collect::<Vec<_>>(),
        [10, 5]
    );
    assert_eq!(history.chart_totals().get("bonus"), Some(&15));
    let provider = ProfileRegistrationProvider::new(database.path(), offline_services());
    let registry = registry_for(&provider);
    let responder = Arc::new(CapturingResponder::default());

    registry
        .command_handler("profile")
        .expect("profile handler")
        .handle(command_request(100, Vec::new()), responder.clone())
        .await
        .expect("profile command");
    registry
        .component_handler("profile:1:economy")
        .expect("profile economy component handler")
        .handle(component_request(100, "economy"), responder.clone())
        .await
        .expect("profile economy component");

    let captured = responder.captured.lock().expect("response capture lock");
    let response = captured
        .edits
        .iter()
        .find(|response| response.embeds[0].title.as_deref() == Some("Profile: 100 > Economy"))
        .expect("economy edit");
    assert_eq!(response.attachments.len(), 1);
    assert_eq!(response.attachments[0].filename, "balance_history.png");
    assert!(
        response.attachments[0]
            .bytes
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
    assert_eq!(
        response.embeds[0].image_url.as_deref(),
        Some("attachment://balance_history.png")
    );
    assert!(
        response.embeds[0]
            .fields
            .iter()
            .any(|field| field.name.contains("Balance History"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dota_component_uploads_lane_chart_on_original_edit() {
    let database = migrated_player_fixture();
    let server = RouteServer::start(5);
    let provider = ProfileRegistrationProvider::new(database.path(), loopback_services(&server));
    let registry = registry_for(&provider);
    let responder = Arc::new(CapturingResponder::default());

    registry
        .command_handler("profile")
        .expect("profile handler")
        .handle(command_request(100, Vec::new()), responder.clone())
        .await
        .expect("profile command");
    registry
        .component_handler("profile:1:dota")
        .expect("profile Dota component handler")
        .handle(component_request(100, "dota"), responder.clone())
        .await
        .expect("profile Dota component");

    assert_eq!(server.wait_for_requests(5).len(), 5);
    let captured = responder.captured.lock().expect("response capture lock");
    let response = captured
        .edits
        .iter()
        .find(|response| response.embeds[0].title.as_deref() == Some("Profile: 100 > Dota Stats"))
        .expect("Dota edit");
    assert_eq!(response.attachments.len(), 1);
    assert_eq!(response.attachments[0].filename, "lane_graph.png");
    assert!(
        response.attachments[0]
            .bytes
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
    assert_eq!(response.embeds[0].image_url, None);
    assert!(response.embeds[0].fields.iter().any(|field| {
        field.name == "Lane Distribution"
            && field.value == "Based on 1 parsed matches\n*(see lane_graph.png below)*"
    }));
}

#[test]
fn test_dota_tab_uses_one_combined_stats_call() {
    let database = migrated_player_fixture();
    let server = RouteServer::start(5);
    let sources = ProfileDataSources::new(database.path(), loopback_services(&server));

    let page = sources
        .dota(100, Some(42), "Discord Display")
        .expect("build Dota profile tab");
    let requests = server.wait_for_requests(5);

    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /players/12345/matches?"))
            .count(),
        1,
        "the shared matches snapshot must feed both role and lane statistics",
    );
    for route in [
        "GET /players/12345 ",
        "GET /players/12345/wl ",
        "GET /players/12345/totals ",
        "GET /players/12345/heroes ",
    ] {
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with(route))
                .count(),
            1,
            "combined Dota stats should request {route:?} exactly once",
        );
    }
    assert!(
        page.fields
            .iter()
            .any(|field| field.name == "Lane Distribution")
    );
}

#[test]
fn test_dota_tab_draws_independent_charts_concurrently() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let chart_job = |marker| {
        let active = Arc::clone(&active);
        let peak = Arc::clone(&peak);
        Box::new(move || {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(current, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_millis(100);
            while active.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
                thread::yield_now();
            }
            thread::sleep(Duration::from_millis(5));
            active.fetch_sub(1, Ordering::SeqCst);
            vec![marker]
        }) as ProfileChartJob<'static>
    };

    let (role, lane) = render_independent_dota_charts(Some(chart_job(b'R')), Some(chart_job(b'L')));

    assert_eq!(role, Some(vec![b'R']));
    assert_eq!(lane, Some(vec![b'L']));
    assert_eq!(peak.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heroes_component_uploads_chart_on_original_edit() {
    let database = migrated_player_fixture();
    insert_heroes_chart_fixture(&database);
    let sources = ProfileDataSources::new(database.path(), offline_services());
    let chart_stats = sources
        .matches
        .get_player_hero_chart_stats(100, Some(42), 20)
        .expect("Heroes chart aggregates");
    assert_eq!(
        chart_stats,
        [
            cama_db::core_repositories::HeroPerformanceStat {
                hero_id: 1,
                games: 2,
                wins: 2,
            },
            cama_db::core_repositories::HeroPerformanceStat {
                hero_id: 2,
                games: 1,
                wins: 0,
            },
        ]
    );
    let provider = ProfileRegistrationProvider::new(database.path(), offline_services());
    let registry = registry_for(&provider);
    let responder = Arc::new(CapturingResponder::default());

    registry
        .command_handler("profile")
        .expect("profile handler")
        .handle(command_request(100, Vec::new()), responder.clone())
        .await
        .expect("profile command");
    registry
        .component_handler("profile:1:heroes")
        .expect("profile Heroes component handler")
        .handle(component_request(100, "heroes"), responder.clone())
        .await
        .expect("profile Heroes component");

    let captured = responder.captured.lock().expect("response capture lock");
    let response = captured
        .edits
        .iter()
        .find(|response| response.embeds[0].title.as_deref() == Some("Profile: 100 > Heroes"))
        .expect("Heroes edit");
    assert_eq!(response.attachments.len(), 1);
    assert_eq!(response.attachments[0].filename, "hero_chart.png");
    assert!(
        response.attachments[0]
            .bytes
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
    assert_eq!(
        response.embeds[0].image_url.as_deref(),
        Some("attachment://hero_chart.png")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_profile_route_uses_real_sqlite_and_loopback_opendota_for_every_tab() {
    let database = migrated_player_fixture();
    let server = RouteServer::start(5);
    let provider = ProfileRegistrationProvider::new(database.path(), loopback_services(&server));
    let registry = registry_for(&provider);
    let command = registry
        .commands()
        .find(|command| command.name == "profile")
        .expect("profile command spec");
    assert_eq!(
        command.description,
        "View unified player profile with tabbed stats"
    );
    assert_eq!(command.options.len(), 1);
    assert_eq!(command.options[0].name, "user");
    assert_eq!(
        command.options[0].description,
        "Player to view profile for (defaults to yourself)"
    );
    assert_eq!(command.options[0].kind, CommandOptionKind::User);
    assert!(!command.options[0].required);
    assert_eq!(registry.component_routes()[0].custom_id_prefix, "profile:");

    let responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("profile")
        .expect("profile command handler")
        .handle(command_request(100, Vec::new()), responder.clone())
        .await
        .expect("execute profile command");

    {
        let captured = responder.captured.lock().expect("response capture lock");
        assert_eq!(captured.deferred, [false]);
        assert!(captured.immediate.is_empty());
        assert_eq!(captured.followups.len(), 1);
        let response = &captured.followups[0];
        assert!(!response.ephemeral);
        assert_eq!(response.embeds[0].title.as_deref(), Some("Profile: 100"));
        assert_eq!(response.components.len(), 2);
        assert_eq!(
            response.components[0]
                .buttons
                .iter()
                .map(|button| button.label.as_str())
                .collect::<Vec<_>>(),
            ["Overview", "Rating", "Economy", "Gambling", "Predictions"]
        );
        assert_eq!(
            response.components[1]
                .buttons
                .iter()
                .map(|button| button.label.as_str())
                .collect::<Vec<_>>(),
            ["Dota", "Teammates", "Heroes", "Roles"]
        );
        assert_eq!(
            response.components[0].buttons[0].style,
            InteractionButtonStyle::Primary
        );
        assert!(
            response
                .components
                .iter()
                .flat_map(|row| &row.buttons)
                .skip(1)
                .all(|button| button.style == InteractionButtonStyle::Secondary)
        );
    }

    let component_handler = registry
        .component_handler("profile:1:dota")
        .expect("profile component handler");
    for (index, tab) in ProfileTab::ALL.into_iter().enumerate() {
        component_handler
            .handle(
                component_request(1_000 + index as u64, tab.key()),
                responder.clone(),
            )
            .await
            .unwrap_or_else(|error| panic!("execute {} tab: {error}", tab.key()));
    }

    let requests = server.wait_for_requests(5);
    assert_eq!(requests.len(), 5);
    assert!(
        requests
            .iter()
            .any(|request| request == "GET /players/12345 HTTP/1.1")
    );
    assert!(
        requests
            .iter()
            .any(|request| request == "GET /players/12345/wl HTTP/1.1")
    );
    assert!(
        requests
            .iter()
            .any(|request| request == "GET /players/12345/totals HTTP/1.1")
    );
    assert!(
        requests
            .iter()
            .any(|request| request == "GET /players/12345/heroes HTTP/1.1")
    );
    assert!(requests.iter().any(|request| {
        request.starts_with("GET /players/12345/matches?")
            && request.contains("limit=50")
            && request.contains("project=hero_id")
    }));

    let captured = responder.captured.lock().expect("response capture lock");
    assert_eq!(captured.edits.len(), 7);
    assert_eq!(captured.updates.len(), 2);
    let pages = captured
        .edits
        .iter()
        .chain(&captured.updates)
        .map(|response| response.embeds[0].title.clone().unwrap_or_default())
        .collect::<Vec<_>>();
    for suffix in [
        "Rating",
        "Economy",
        "Gambling",
        "Predictions",
        "Dota Stats",
        "Teammates",
        "Heroes",
        "Roles",
    ] {
        assert!(
            pages
                .iter()
                .any(|title| title == &format!("Profile: 100 > {suffix}")),
            "missing {suffix} live page from {pages:?}"
        );
    }
    let dota = captured
        .edits
        .iter()
        .find(|response| response.embeds[0].title.as_deref() == Some("Profile: 100 > Dota Stats"))
        .expect("Dota edit");
    assert_eq!(
        dota.embeds[0].footer.as_deref(),
        Some("Data from OpenDota | Based on last 50 matches")
    );
    assert!(
        dota.embeds[0]
            .fields
            .iter()
            .any(|field| { field.name == "OpenDota Win Rate" && field.value == "60.0% (last 50)" })
    );
    assert!(dota.embeds[0].fields.iter().any(|field| {
        field.name == "Top Heroes" && field.value.contains("**Anti-Mage** - 10g (60%)")
    }));
}

#[tokio::test]
async fn registered_profile_uses_discord_render_name_instead_of_database_username() {
    let database = migrated_player_fixture();
    let provider = ProfileRegistrationProvider::new(database.path(), offline_services())
        .with_player_names(Arc::new(FixedProfileNames));
    let responder = Arc::new(CapturingResponder::default());

    registry_for(&provider)
        .command_handler("profile")
        .expect("profile handler")
        .handle(command_request(100, Vec::new()), responder.clone())
        .await
        .expect("profile response");

    let captured = responder.captured.lock().expect("response capture lock");
    assert_eq!(
        captured.followups[0].embeds[0].title.as_deref(),
        Some("Profile: Discord Display")
    );
}

#[tokio::test]
async fn selected_unregistered_profile_uses_discord_id_without_cached_member() {
    let database = migrated_player_fixture();
    let server = RouteServer::start(0);
    let provider = ProfileRegistrationProvider::new(database.path(), loopback_services(&server));
    let registry = registry_for(&provider);
    let responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("profile")
        .expect("profile handler")
        .handle(
            command_request(
                100,
                vec![InteractionOption {
                    name: "user".to_owned(),
                    value: InteractionValue::User {
                        id: 999,
                        display_name: Some("Selected Name".to_owned()),
                        is_bot: Some(false),
                    },
                }],
            ),
            responder.clone(),
        )
        .await
        .expect("unregistered response");
    let captured = responder.captured.lock().expect("response capture lock");
    assert_eq!(captured.deferred, [false]);
    assert_eq!(captured.followups.len(), 1);
    assert_eq!(captured.followups[0].content, "");
    assert_eq!(captured.followups[0].embeds.len(), 1);
    assert_eq!(
        captured.followups[0].embeds[0].title.as_deref(),
        Some("Not Registered")
    );
    assert_eq!(
        captured.followups[0].embeds[0].description.as_deref(),
        Some("999 is not registered.\nUse `/player register` to get started.")
    );
    assert_eq!(captured.followups[0].embeds[0].color, Some(0xE7_4C_3C));
    assert!(captured.immediate.is_empty());
}

#[tokio::test]
async fn sixth_profile_command_is_rejected_privately_before_defer() {
    let database = migrated_player_fixture();
    let server = RouteServer::start(0);
    let provider = ProfileRegistrationProvider::new(database.path(), loopback_services(&server));
    let registry = registry_for(&provider);
    let handler = registry
        .command_handler("profile")
        .expect("profile handler");
    let responder = Arc::new(CapturingResponder::default());
    for interaction_id in 0..6_u64 {
        let mut request = command_request(
            777,
            vec![InteractionOption {
                name: "user".to_owned(),
                value: InteractionValue::User {
                    id: 999,
                    display_name: Some("Missing".to_owned()),
                    is_bot: Some(false),
                },
            }],
        );
        if let InteractionRequest::Command {
            interaction_id: id, ..
        } = &mut request
        {
            *id = interaction_id;
        }
        handler
            .handle(request, responder.clone())
            .await
            .expect("profile rate-limit call");
    }
    let captured = responder.captured.lock().expect("response capture lock");
    assert_eq!(captured.deferred, [false; 5]);
    assert_eq!(captured.followups.len(), 5);
    assert_eq!(captured.immediate.len(), 1);
    assert!(captured.immediate[0].ephemeral);
    assert_eq!(
        captured.immediate[0].content,
        "⏳ Please wait 30s before using `/profile` again."
    );
}

#[tokio::test]
async fn profile_view_expires_deletes_message_and_rejects_late_buttons() {
    let database = migrated_player_fixture();
    let server = RouteServer::start(0);
    let provider = ProfileRegistrationProvider::with_timeout(
        database.path(),
        loopback_services(&server),
        Duration::from_millis(10),
    );
    let registry = registry_for(&provider);
    let responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("profile")
        .expect("profile handler")
        .handle(command_request(100, Vec::new()), responder.clone())
        .await
        .expect("profile command");
    tokio::time::sleep(Duration::from_millis(30)).await;
    registry
        .component_handler("profile:1:overview")
        .expect("profile component handler")
        .handle(component_request(200, "overview"), responder.clone())
        .await
        .expect("expired component");

    let captured = responder.captured.lock().expect("response capture lock");
    assert_eq!(
        captured.deleted,
        [InteractionMessageReceipt {
            message_id: 77,
            channel_id: 9,
            delivery: InteractionMessageDelivery::InteractionFollowup,
        }]
    );
    assert_eq!(
        captured.immediate.last(),
        Some(
            &InteractionResponse::message("This profile view has expired. Run `/profile` again.")
                .ephemeral()
        )
    );
}

#[test]
fn test_teammates_embed_unregistered_user() {
    let database = migrated_player_fixture();
    let page = teammates_page(&database, 99_999, "UnregisteredUser");

    assert_eq!(page.title, "Not Registered");
    assert!(
        page.description
            .as_deref()
            .is_some_and(|description| description.to_lowercase().contains("not registered"))
    );
    assert!(page.attachments.is_empty());
}

#[test]
fn test_teammates_embed_no_data() {
    let database = migrated_player_fixture();
    let page = teammates_page(&database, 100, "Database Name");

    assert!(page.title.contains("Teammates"));
    assert_eq!(
        teammates_field(&page, "Best Teammates").value,
        "No data yet"
    );
    assert_eq!(
        teammates_field(&page, "Worst Teammates").value,
        "No data yet"
    );
    assert_eq!(page.footer.as_deref(), Some("Min 3 games"));
    assert!(page.attachments.is_empty());
}

#[test]
fn test_teammates_embed_with_data() {
    let database = migrated_player_fixture();
    insert_profile_pairing(&database, 200, 4, 4, 0, 0);
    insert_profile_pairing(&database, 300, 4, 0, 0, 0);
    let page = teammates_page(&database, 100, "Database Name");

    let best = &teammates_field(&page, "Best Teammates").value;
    assert!(best.contains("<@200>"));
    assert!(best.contains("100%"));
    let worst = &teammates_field(&page, "Worst Teammates").value;
    assert!(worst.contains("<@300>"));
    assert!(worst.contains("0%"));
    assert!(
        page.footer
            .as_deref()
            .is_some_and(|footer| footer.contains("Min 3 games"))
    );
}

#[test]
fn test_teammates_embed_dominates_and_struggles_sections() {
    let database = migrated_player_fixture();
    insert_profile_pairing(&database, 200, 0, 0, 4, 4);
    insert_profile_pairing(&database, 300, 0, 0, 4, 0);
    let page = teammates_page(&database, 100, "Database Name");

    let dominates = &teammates_field(&page, "Dominates").value;
    assert!(dominates.contains("<@200>"));
    assert!(dominates.contains("100%"));
    let struggles = &teammates_field(&page, "Struggles").value;
    assert!(struggles.contains("<@300>"));
    assert!(struggles.contains("0%"));
}

#[test]
fn test_teammates_embed_most_played_sections() {
    let database = migrated_player_fixture();
    insert_profile_pairing(&database, 200, 6, 6, 0, 0);
    insert_profile_pairing(&database, 300, 0, 0, 6, 6);
    let page = teammates_page(&database, 100, "Database Name");

    let most_with = &teammates_field(&page, "Most Played With").value;
    assert!(most_with.contains("<@200>"));
    assert!(most_with.contains("6g"));
    let most_against = &teammates_field(&page, "Most Played Against").value;
    assert!(most_against.contains("<@300>"));
    assert!(most_against.contains("6g"));
}

#[test]
fn test_teammates_embed_evenly_matched() {
    let database = migrated_player_fixture();
    insert_profile_pairing(&database, 200, 4, 2, 0, 0);
    let page = teammates_page(&database, 100, "Database Name");

    let even = &teammates_field(&page, "Even Teammates").value;
    assert_ne!(even, "No data yet");
    assert!(even.contains("<@200>"));
    assert!(even.contains("2W/2L"));
}

#[test]
fn test_teammates_embed_min_games_threshold() {
    let database = migrated_player_fixture();
    insert_profile_pairing(&database, 200, 2, 2, 0, 0);
    let page = teammates_page(&database, 100, "Database Name");

    assert_eq!(
        teammates_field(&page, "Best Teammates").value,
        "No data yet"
    );
}

#[test]
fn test_teammates_embed_no_pairings_repo() {
    let database = migrated_player_fixture();
    Connection::open(database.path())
        .expect("open profile fixture")
        .execute("DROP TABLE player_pairings", [])
        .expect("remove pairings storage");
    let page = teammates_page(&database, 100, "Database Name");

    assert_eq!(page.title, "Error");
    assert!(
        page.description
            .as_deref()
            .is_some_and(|description| description.to_lowercase().contains("unavailable"))
    );
}

#[test]
fn test_economy_tab_empty_history_returns_no_chart() {
    let database = migrated_player_fixture();
    let page = profile_page(&database, ProfileTab::Economy);

    assert!(page.title.ends_with("> Economy"));
    assert!(page.attachments.is_empty());
    assert!(
        page.fields
            .iter()
            .any(|field| field.name.contains("Balance"))
    );
    assert!(
        !page
            .fields
            .iter()
            .any(|field| field.name.contains("Balance History"))
    );
}

#[test]
fn test_economy_tab_unregistered_player_short_circuits() {
    let database = migrated_player_fixture();
    let page = ProfileDataSources::new(database.path(), offline_services())
        .economy(999_999, Some(42), "Ghost")
        .expect("build unregistered economy page");

    assert_eq!(page.title, "Not Registered");
    assert!(page.description.as_deref().is_some_and(|description| {
        description.contains("Ghost") && description.contains("not registered")
    }));
    assert!(page.attachments.is_empty());
}

#[test]
fn test_economy_tab_without_balance_history_service() {
    let database = migrated_player_fixture();
    let page = profile_page(&database, ProfileTab::Economy);

    // The Rust runtime owns this adapter in its data-source bundle; an empty
    // adapter is the equivalent of Python's absent optional service.
    assert!(page.attachments.is_empty());
    assert!(!page.title.is_empty());
}

#[test]
fn test_economy_tab_loads_independent_sections_concurrently() {
    let database = migrated_player_fixture();
    let connection = Connection::open(database.path()).expect("open economy fixture");
    connection
        .execute(
            "INSERT INTO loan_state (
                 discord_id, guild_id, total_loans_taken, total_fees_paid,
                 negative_loans_taken, outstanding_principal, outstanding_fee
             ) VALUES (100, 42, 2, 10, 1, 30, 3)",
            [],
        )
        .expect("insert loan state");
    connection
        .execute(
            "INSERT INTO bankruptcy_state (
                 discord_id, guild_id, bankruptcy_count, penalty_games_remaining
             ) VALUES (100, 42, 2, 0)",
            [],
        )
        .expect("insert bankruptcy state");
    drop(connection);
    let page = profile_page(&database, ProfileTab::Economy);

    assert!(page.fields.iter().any(|field| field.name.contains("Loans")));
    assert!(
        page.fields
            .iter()
            .any(|field| field.name.contains("Bankruptcy"))
    );
    assert!(
        page.fields
            .iter()
            .find(|field| field.name.contains("Bankruptcy"))
            .is_some_and(|field| field.value.contains("Declarations:** 2"))
    );
}

#[test]
fn test_auto_bet_field_is_omitted_without_automatic_bet_history() {
    let mut page = ProfilePage::new("Gambling", DISCORD_BLUE);
    append_auto_bet_field(&mut page, &AutoBetPerformance::default());

    assert!(page.fields.is_empty());
}

#[test]
fn test_auto_bet_field_respects_remaining_total_embed_budget() {
    let mut page = ProfilePage::new("Gambling", DISCORD_BLUE)
        .description("x".repeat(4_096))
        .field("Existing A", "a".repeat(900), false)
        .field("Existing B", "b".repeat(800), false);
    append_auto_bet_field(&mut page, &sample_auto_bet_performance(20));

    let field = page.fields.last().expect("auto-bet field");
    assert_eq!(field.name, "Auto-Bet P&L");
    assert!(field.value.contains("omitted"));
    assert!(field.value.chars().count() <= PROFILE_FIELD_VALUE_LIMIT);
    assert!(profile_text_len(&page) <= PROFILE_EMBED_TOTAL_LIMIT);
}

#[test]
fn test_gambling_profile_renders_budgeted_auto_bet_pnl_with_omitted_targets() {
    let mut page = ProfilePage::new("Profile: Investor > Gambling", DISCORD_GREEN);
    append_auto_bet_field(&mut page, &sample_auto_bet_performance(80));

    let field = page.fields.last().expect("auto-bet field");
    assert_eq!(field.name, "Auto-Bet P&L");
    assert!(field.value.contains("**Auto total:** +12,345 JC"));
    assert!(field.value.contains("**Generic auto:** -7,890 JC"));
    assert!(field.value.contains("**Arbitrage hedges:** +4,567 JC"));
    assert!(field.value.contains("<@9000000000000000000> LONG"));
    assert!(field.value.contains("<@9000000000000000001> SHORT"));
    assert!(field.value.contains("more targets omitted"));
    assert!(field.value.chars().count() <= PROFILE_FIELD_VALUE_LIMIT);
}

#[test]
fn test_heroes_tab_runs_independent_reads_and_drawing_concurrently() {
    let database = migrated_player_fixture();
    insert_heroes_chart_fixture(&database);
    let page = profile_page(&database, ProfileTab::Heroes);

    assert_eq!(page.title, "Profile: Player > Heroes");
    assert!(
        page.fields
            .iter()
            .any(|field| field.name == "Overview" && field.value.contains("Avg GPM"))
    );
    assert_eq!(page.attachments.len(), 1);
    assert_eq!(page.attachments[0].filename, "hero_chart.png");
}

#[test]
fn test_overview_loads_badge_bankruptcy_and_hero_stats_concurrently() {
    let database = migrated_player_fixture();
    Connection::open(database.path())
        .expect("open overview fixture")
        .execute(
            "UPDATE players SET os_mu = 30.0, os_sigma = 3.0
             WHERE discord_id = 100 AND guild_id = 42",
            [],
        )
        .expect("set overview OpenSkill fixture");
    let page = profile_page(&database, ProfileTab::Overview);
    let rating = page
        .fields
        .iter()
        .find(|field| field.name == "Rating")
        .expect("overview rating field");

    assert_eq!(page.title, "Profile: Player");
    assert!(rating.value.contains("**OpenSkill:** 250"));
    assert!(rating.value.contains("64% certain"));
}

#[test]
fn test_predictions_loads_stats_and_positions_concurrently() {
    let database = migrated_player_fixture();
    let page = prediction_page(&database);

    assert!(page.attachments.is_empty());
    assert!(
        page.description
            .as_deref()
            .is_some_and(|description| { description.contains("No prediction market activity") })
    );
}

#[test]
fn test_gambling_loads_balance_impact_and_chart_series_concurrently() {
    let database = migrated_player_fixture();
    insert_gambling_chart_fixture(&database);
    let page = profile_page(&database, ProfileTab::Gambling);

    assert_eq!(page.title, "Profile: Player > Gambling");
    assert!(page.fields.iter().any(|field| field.name == "Performance"));
    assert_eq!(page.attachments.len(), 1);
    assert_eq!(page.attachments[0].filename, "gamba_chart.png");
}

#[test]
fn test_gambling_does_not_attach_chart_for_one_event() {
    let database = migrated_player_fixture();
    insert_gambling_chart_fixture_for(
        &database,
        &[(10_i64, 1_i64, 1_700_000_000_i64, Some(20_i64))],
    );

    let page = profile_page(&database, ProfileTab::Gambling);
    assert_eq!(page.title, "Profile: Player > Gambling");
    assert!(page.attachments.is_empty());
}

#[test]
fn test_rating_loads_population_and_history_concurrently() {
    let database = migrated_player_fixture();
    let page = profile_page(&database, ProfileTab::Rating);

    assert_eq!(page.title, "Profile: Player > Rating");
    assert!(page.fields.iter().any(|field| field.name == "OpenSkill"));
    assert!(page.attachments.is_empty());
}

#[test]
fn test_predictions_tab_empty() {
    let database = migrated_player_fixture();
    let page = prediction_page(&database);

    assert!(page.fields.is_empty());
    assert!(
        page.description
            .as_deref()
            .is_some_and(|description| { description.contains("/predict list") })
    );
}

#[test]
fn test_predictions_tab_open_position() {
    let database = migrated_player_fixture();
    let (repository, prediction_id) = insert_profile_prediction(&database, "Will it rain?");
    let receipt = repository
        .buy_contracts_atomic(prediction_id, 100, ContractSide::Yes, 3)
        .expect("buy profile prediction contracts");
    let page = prediction_page(&database);

    let positions = page
        .fields
        .iter()
        .find(|field| field.name.contains("Open Positions"))
        .expect("open positions field");
    assert!(positions.value.contains(&format!("#{prediction_id}")));
    assert!(positions.value.contains("Will it rain?"));
    assert!(positions.value.contains("YES 3 @ 50%"));
    assert!(receipt.total > 0);
    assert!(
        page.fields
            .iter()
            .find(|field| field.name == "Performance")
            .is_none_or(|field| field.value.contains("0W-0L"))
    );
}

#[test]
fn test_predictions_tab_resolved_win() {
    let database = migrated_player_fixture();
    let (repository, prediction_id) = insert_profile_prediction(&database, "Will it snow?");
    let receipt = repository
        .buy_contracts_atomic(prediction_id, 100, ContractSide::Yes, 3)
        .expect("buy winning prediction contracts");
    repository
        .settle_orderbook(prediction_id, ContractSide::Yes, Some(100))
        .expect("settle winning prediction");
    let page = prediction_page(&database);

    let performance = page
        .fields
        .iter()
        .find(|field| field.name == "Performance")
        .expect("prediction performance field");
    let expected = 3 * CONTRACT_VALUE - receipt.total;
    assert!(performance.value.contains(&format!("+{expected}")));
    assert!(performance.value.contains("1W-0L"));
}

#[test]
fn test_predictions_tab_resolved_loss() {
    let database = migrated_player_fixture();
    let (repository, prediction_id) = insert_profile_prediction(&database, "Will it hail?");
    let receipt = repository
        .buy_contracts_atomic(prediction_id, 100, ContractSide::Yes, 3)
        .expect("buy losing prediction contracts");
    repository
        .settle_orderbook(prediction_id, ContractSide::No, Some(100))
        .expect("settle losing prediction");
    let page = prediction_page(&database);

    let performance = page
        .fields
        .iter()
        .find(|field| field.name == "Performance")
        .expect("prediction performance field");
    assert!(performance.value.contains(&format!("-{}", receipt.total)));
    assert!(performance.value.contains("0W-1L"));
}

#[test]
fn exact_component_rows_and_active_style_match_python_profile_view() {
    let rows = profile_action_rows(42, ProfileTab::Predictions);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].buttons.len(), 5);
    assert_eq!(rows[1].buttons.len(), 4);
    assert_eq!(
        rows.iter()
            .flat_map(|row| &row.buttons)
            .map(|button| button.custom_id.as_str())
            .collect::<Vec<_>>(),
        [
            "profile:42:overview",
            "profile:42:rating",
            "profile:42:economy",
            "profile:42:gambling",
            "profile:42:predictions",
            "profile:42:dota",
            "profile:42:teammates",
            "profile:42:heroes",
            "profile:42:roles",
        ]
    );
    assert_eq!(
        rows.iter()
            .flat_map(|row| &row.buttons)
            .filter(|button| button.style == InteractionButtonStyle::Primary)
            .map(|button| button.label.as_str())
            .collect::<Vec<_>>(),
        ["Predictions"]
    );
}
