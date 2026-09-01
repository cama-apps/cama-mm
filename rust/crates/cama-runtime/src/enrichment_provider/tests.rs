use super::*;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;
use std::thread;
use std::time::{Duration, Instant};

use cama_app::opendota_http::OpenDotaHttpConfig;
use cama_db::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
use rusqlite::{Connection, params};
use tempfile::{NamedTempFile, TempDir};

use crate::registration::{
    InteractionMessageDelivery, InteractionMessageReceipt, InteractionResponseError, Registry,
};

const ADMIN: u64 = 42;
const GUILD: u64 = 4_242;

#[test]
fn parsed_refresh_run_guard_excludes_overlap_and_releases_on_drop() {
    let running = AtomicBool::new(false);
    let first = try_claim_parsed_refresh(&running).expect("first refresh claims the guard");
    assert!(try_claim_parsed_refresh(&running).is_none());
    drop(first);
    assert!(try_claim_parsed_refresh(&running).is_some());
}

#[derive(Default)]
struct CapturedResponses {
    immediate: Vec<InteractionResponse>,
    deferred: Vec<bool>,
    followups: Vec<InteractionResponse>,
    original_edits: Vec<InteractionResponse>,
    message_edits: Vec<(InteractionMessageReceipt, InteractionResponse)>,
    updates: Vec<InteractionResponse>,
}

#[derive(Default)]
struct CapturingResponder {
    captured: StdMutex<CapturedResponses>,
    receipt: StdMutex<Option<InteractionMessageReceipt>>,
}

#[test]
fn match_view_timeout_edit_is_component_only_and_preserves_body_media() {
    let response = InteractionResponse::message("match summary")
        .embed(InteractionEmbed::titled("Match #7"))
        .attachment(InteractionAttachment::bytes("advantage.png", vec![1, 2, 3]))
        .action_row(InteractionActionRow::buttons(vec![InteractionButton::new(
            "enrichment:match:4242:7:999:1",
            "Next >",
        )]));

    let timeout = disabled_component_response(&response).expect("view has controls");
    assert!(timeout.content.is_empty());
    assert!(timeout.embeds.is_empty());
    assert!(timeout.attachments.is_empty());
    assert_eq!(timeout.components.len(), 1);
    assert!(timeout.components[0].buttons[0].disabled);
}

#[async_trait]
impl InteractionResponder for CapturingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
            .immediate
            .push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
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
            .expect("response capture")
            .followups
            .push(response);
        Ok(())
    }

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        self.followup(response).await?;
        Ok(*self.receipt.lock().expect("response receipt"))
    }

    async fn edit_original(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
            .original_edits
            .push(response);
        Ok(())
    }

    async fn edit_message(
        &self,
        receipt: InteractionMessageReceipt,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
            .message_edits
            .push((receipt, response));
        Ok(())
    }

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
            .updates
            .push(response);
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct RouteServer {
    base_url: String,
    requests: Arc<StdMutex<Vec<String>>>,
}

impl RouteServer {
    fn start(bodies: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback OpenDota");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("loopback address")
        );
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        thread::spawn(move || {
            for body in bodies {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let Some(request) = read_request(&mut stream) else {
                    return;
                };
                captured.lock().expect("request capture").push(request);
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(wire.as_bytes());
            }
        });
        Self { base_url, requests }
    }

    fn requests(&self, expected: usize) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let requests = self.requests.lock().expect("request capture").clone();
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

fn migrated() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("temporary enrichment database");
    let path = directory.path().join("cama.sqlite3");
    initialize_or_migrate_with_settings(&path, &MigrationSettings::default())
        .expect("migrate enrichment database");
    (directory, path)
}

fn dotabase() -> NamedTempFile {
    let file = NamedTempFile::new().expect("temporary Dotabase catalog");
    let connection = Connection::open(file.path()).expect("open Dotabase fixture");
    connection
        .execute_batch(
            "CREATE TABLE heroes (
                 id INTEGER PRIMARY KEY,
                 name TEXT,
                 localized_name TEXT,
                 real_name TEXT,
                 hype TEXT,
                 bio TEXT,
                 attr_primary TEXT,
                 is_melee INTEGER,
                 base_movement INTEGER,
                 base_armor REAL,
                 attack_rate REAL,
                 attr_strength_base REAL,
                 attr_agility_base REAL,
                 attr_intelligence_base REAL,
                 attr_strength_gain REAL,
                 attr_agility_gain REAL,
                 attr_intelligence_gain REAL,
                 vision_night INTEGER,
                 turn_rate REAL,
                 attack_damage_min INTEGER,
                 attack_damage_max INTEGER
             );
             INSERT INTO heroes(id,name,localized_name,is_melee)
             VALUES (1,'npc_dota_hero_antimage','Anti-Mage',1),
                    (2,'npc_dota_hero_axe','Axe',1);",
        )
        .expect("create Dotabase hero fixture");
    file
}

fn application_config() -> ApplicationConfig {
    ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some(ADMIN.to_string()),
        "ENRICHMENT_HISTORY_LIMIT" => Some("500".to_owned()),
        "ENRICHMENT_MIN_PLAYER_MATCH" => Some("5".to_owned()),
        // Production deliberately paces repair traffic. Tests exercise the
        // same orchestration without waiting between loopback requests.
        "ENRICHMENT_REFRESH_INTERVAL_MS" => Some("0".to_owned()),
        "ENRICHMENT_RETRY_DELAYS" => Some("0".to_owned()),
        _ => None,
    })
    .expect("enrichment test configuration")
}

fn services(server: &RouteServer) -> Arc<OpenDotaRuntimeServices> {
    let mut config = OpenDotaHttpConfig::new(&server.base_url, None);
    config.request_timeout = Duration::from_secs(2);
    config.request_deadline = Duration::from_secs(5);
    config.retry_delays = vec![Duration::ZERO];
    config.requests_per_minute = 1_200;
    Arc::new(OpenDotaRuntimeServices::with_config(config).expect("loopback OpenDota services"))
}

fn offline_services() -> Arc<OpenDotaRuntimeServices> {
    let mut config = OpenDotaHttpConfig::new("http://127.0.0.1:9", None);
    config.request_timeout = Duration::from_millis(1);
    config.request_deadline = Duration::from_millis(1);
    config.retry_delays = vec![Duration::ZERO];
    Arc::new(OpenDotaRuntimeServices::with_config(config).expect("offline OpenDota services"))
}

fn registry(provider: &EnrichmentRegistrationProvider) -> Registry {
    let mut builder = RegistryBuilder::default();
    provider
        .register(&mut builder)
        .expect("register enrichment provider");
    builder.build()
}

fn command(
    root: &str,
    subcommand: &str,
    options: Vec<InteractionOption>,
    user_id: u64,
    guild_id: Option<u64>,
    permissions: Option<u64>,
) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 9,
        name: root.to_owned(),
        user_id,
        user_display_name: format!("Player {user_id}"),
        guild_id,
        channel_id: Some(7),
        member_permissions: permissions,
        options: vec![InteractionOption {
            name: subcommand.to_owned(),
            value: InteractionValue::Subcommand(options),
        }],
    }
}

fn option(name: &str, value: InteractionValue) -> InteractionOption {
    InteractionOption {
        name: name.to_owned(),
        value,
    }
}

fn manual_match_payload() -> String {
    r#"{
        "match_id":9001,
        "duration":2400,
        "radiant_win":true,
        "radiant_score":35,
        "dire_score":22,
        "game_mode":2,
        "radiant_gold_adv":[0,100,250],
        "radiant_xp_adv":[0,-50,125],
        "comeback":123,
        "throw":456,
        "picks_bans":[
            {"is_pick":false,"team":0,"hero_id":44},
            {"is_pick":true,"team":1,"hero_id":55},
            {"is_pick":0,"team":"1","hero_id":"66"}
        ],
        "players":[{
            "account_id":12345,
            "player_slot":0,
            "hero_id":1,
            "kills":10,
            "deaths":2,
            "assists":5,
            "gold_per_min":600,
            "xp_per_min":550,
            "hero_damage":25000,
            "tower_damage":3000,
            "last_hits":200,
            "denies":10,
            "net_worth":20000,
            "hero_healing":100,
            "lane_role":2,
            "lane_efficiency_pct":71,
            "towers_killed":3,
            "roshans_killed":1,
            "teamfight_participation":0.7,
            "obs_placed":2,
            "sen_placed":3,
            "camps_stacked":4,
            "rune_pickups":5,
            "firstblood_claimed":true,
            "stuns":12.5,
            "actions_per_min":200,
            "courier_kills":1,
            "pings":12,
            "purchase_log":[{"key":"rapier"}]
        }]
    }"#
    .to_owned()
}

fn strict_manual_match_payload() -> String {
    let mut payload: serde_json::Value =
        serde_json::from_str(&manual_match_payload()).expect("manual match fixture is JSON");
    let players = payload
        .get_mut("players")
        .and_then(serde_json::Value::as_array_mut)
        .expect("manual match players");
    let template = players[0].clone();
    for offset in 1_i64..10 {
        let mut player = template.clone();
        player["account_id"] = serde_json::json!(12_345 + offset);
        player["player_slot"] =
            serde_json::json!(if offset < 5 { offset } else { 128 + offset - 5 });
        player["kills"] = serde_json::json!(0);
        player["assists"] = serde_json::json!(0);
        players.push(player);
    }
    serde_json::to_string(&payload).expect("serialize strict manual match fixture")
}

fn parsed_match_payload() -> String {
    let mut payload: serde_json::Value =
        serde_json::from_str(&manual_match_payload()).expect("manual match fixture is JSON");
    let player = payload
        .get_mut("players")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|players| players.first_mut())
        .expect("manual match fixture has a player");
    player["gold_t"] = serde_json::json!([0, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1000]);
    player["lh_t"] = serde_json::json!([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    serde_json::to_string(&payload).expect("serialize parsed match fixture")
}

fn seed_manual_match(path: &std::path::Path) {
    let connection = Connection::open(path).expect("open enrichment fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username,steam_id)
             VALUES (?1,?2,'Radiant',12345)",
            params![100_i64, GUILD as i64],
        )
        .expect("insert player");
    connection
        .execute(
            "INSERT INTO player_steam_ids(discord_id,steam_id,is_primary,added_at)
             VALUES (100,12345,1,1700000000)",
            [],
        )
        .expect("insert Steam link");
    connection
        .execute(
            "INSERT INTO matches(
                 match_id,guild_id,team1_players,team2_players,winning_team,match_date
             ) VALUES (7,?1,'[100]','[]',1,'2024-01-15 12:00:00')",
            [GUILD as i64],
        )
        .expect("insert match");
    connection
        .execute(
            "INSERT INTO match_participants(
                 match_id,discord_id,guild_id,team_number,won,side
             ) VALUES (7,100,?1,1,1,'radiant')",
            [GUILD as i64],
        )
        .expect("insert participant");
}

fn complete_manual_roster(path: &std::path::Path) {
    let mut connection = Connection::open(path).expect("open strict manual fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("strict manual foreign-key mode");
    let transaction = connection.transaction().expect("strict manual transaction");
    for offset in 1_i64..10 {
        let discord_id = 100 + offset;
        let steam_id = 12_345 + offset;
        transaction
            .execute(
                "INSERT INTO players(discord_id,guild_id,discord_username,steam_id)
                 VALUES (?1,?2,?3,?4)",
                params![
                    discord_id,
                    GUILD as i64,
                    format!("Roster {offset}"),
                    steam_id
                ],
            )
            .expect("insert strict roster player");
        transaction
            .execute(
                "INSERT INTO player_steam_ids(discord_id,steam_id,is_primary,added_at)
                 VALUES (?1,?2,1,1700000000)",
                params![discord_id, steam_id],
            )
            .expect("insert strict roster Steam link");
        let radiant = offset < 5;
        transaction
            .execute(
                "INSERT INTO match_participants(
                     match_id,discord_id,guild_id,team_number,won,side
                 ) VALUES (7,?1,?2,?3,?4,?5)",
                params![
                    discord_id,
                    GUILD as i64,
                    if radiant { 1 } else { 2 },
                    i64::from(radiant),
                    if radiant { "radiant" } else { "dire" }
                ],
            )
            .expect("insert strict roster participant");
    }
    transaction
        .execute(
            "UPDATE matches
             SET team1_players='[100,101,102,103,104]',
                 team2_players='[105,106,107,108,109]'
             WHERE match_id=7",
            [],
        )
        .expect("complete strict roster arrays");
    transaction.commit().expect("commit strict manual roster");
}

fn seed_enriched_view_match(path: &std::path::Path) {
    seed_manual_match(path);
    let connection = Connection::open(path).expect("open enriched view fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username)
             VALUES (101,?1,'Selected Player')",
            [GUILD as i64],
        )
        .expect("insert selected player");
    connection
        .execute(
            "UPDATE matches
             SET team1_players='[100,101]', valve_match_id=9001,
                 duration_seconds=2400, radiant_score=35, dire_score=22,
                 lobby_type='draft', enrichment_source='manual',
                 enrichment_data=?1
             WHERE match_id=7",
            [r#"{"radiant_gold_adv":[0,100],"radiant_xp_adv":[0,50]}"#],
        )
        .expect("enrich view match");
    connection
        .execute(
            "UPDATE match_participants
             SET hero_id=1,kills=1,deaths=2,assists=3,hero_damage=100
             WHERE match_id=7 AND discord_id=100",
            [],
        )
        .expect("enrich invoking participant");
    connection
        .execute(
            "INSERT INTO match_participants(
                 match_id,discord_id,guild_id,team_number,won,side,hero_id,
                 kills,deaths,assists,hero_damage
             ) VALUES (7,101,?1,1,1,'radiant',2,10,0,8,50000)",
            [GUILD as i64],
        )
        .expect("insert MVP participant");
}

fn seed_discovery_match(path: &std::path::Path) {
    let connection = Connection::open(path).expect("open discovery fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    for offset in 0_i64..10 {
        let discord_id = 300 + offset;
        let steam_id = 20_000 + offset;
        connection
            .execute(
                "INSERT INTO players(discord_id,guild_id,discord_username,steam_id)
                 VALUES (?1,?2,?3,?4)",
                params![
                    discord_id,
                    GUILD as i64,
                    format!("Discovery {offset}"),
                    steam_id
                ],
            )
            .expect("insert discovery player");
        connection
            .execute(
                "INSERT INTO player_steam_ids(discord_id,steam_id,is_primary,added_at)
                 VALUES (?1,?2,1,1700000000)",
                params![discord_id, steam_id],
            )
            .expect("insert discovery Steam link");
    }
    connection
        .execute(
            "INSERT INTO matches(
                 match_id,guild_id,team1_players,team2_players,winning_team,match_date
             ) VALUES (
                 8,?1,'[300,301,302,303,304]','[305,306,307,308,309]',1,'2024-01-15 12:00:00'
             )",
            [GUILD as i64],
        )
        .expect("insert discovery match");
    for offset in 0_i64..10 {
        let side = if offset < 5 { "radiant" } else { "dire" };
        connection
            .execute(
                "INSERT INTO match_participants(
                     match_id,discord_id,guild_id,team_number,won,side
                 ) VALUES (8,?1,?2,?3,?4,?5)",
                params![
                    300 + offset,
                    GUILD as i64,
                    i64::from(offset >= 5) + 1,
                    i64::from(offset < 5),
                    side
                ],
            )
            .expect("insert discovery participant");
    }
}

fn discovery_match_payload() -> String {
    let players = (0_i64..10)
        .map(|offset| {
            serde_json::json!({
                "account_id": 20_000 + offset,
                "player_slot": if offset < 5 { offset } else { 128 + offset - 5 },
                "hero_id": offset + 1,
                "kills": 5 + offset,
                "deaths": 2,
                "assists": 9,
                "gold_per_min": 500,
                "xp_per_min": 475,
                "net_worth": 18_000,
                "lane_role": 2,
                "actions_per_min": 150 + offset
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "match_id": 9_002,
        "duration": 2_100,
        "radiant_win": true,
        "radiant_score": 31,
        "dire_score": 18,
        "game_mode": 2,
        "players": players
    })
    .to_string()
}

fn seed_pending_openskill_replay(path: &std::path::Path) {
    let connection = Connection::open(path).expect("open OpenSkill recovery fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    let radiant = (500_i64..505).collect::<Vec<_>>();
    let dire = (505_i64..510).collect::<Vec<_>>();
    for discord_id in radiant.iter().chain(&dire) {
        connection
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,initial_mmr,os_mu,os_sigma
                 ) VALUES (?1,?2,?3,4000,NULL,NULL)",
                params![discord_id, GUILD as i64, format!("Replay {discord_id}")],
            )
            .expect("insert replay player");
    }
    connection
        .execute(
            "INSERT INTO matches(
                 match_id,guild_id,team1_players,team2_players,winning_team,match_date,
                 valve_match_id,enrichment_source
             ) VALUES (9,?1,?2,?3,1,'2024-01-16 12:00:00',9003,'manual')",
            params![
                GUILD as i64,
                serde_json::to_string(&radiant).unwrap(),
                serde_json::to_string(&dire).unwrap()
            ],
        )
        .expect("insert replay match");
    for (index, discord_id) in radiant.iter().chain(&dire).enumerate() {
        let radiant_player = index < 5;
        connection
            .execute(
                "INSERT INTO match_participants(
                     match_id,discord_id,guild_id,team_number,won,side,fantasy_points
                 ) VALUES (9,?1,?2,?3,?4,?5,?6)",
                params![
                    discord_id,
                    GUILD as i64,
                    if radiant_player { 1 } else { 2 },
                    i64::from(radiant_player),
                    if radiant_player { "radiant" } else { "dire" },
                    10.0 + index as f64
                ],
            )
            .expect("insert replay participant");
    }
    connection
        .execute(
            "INSERT INTO openskill_replay_jobs(guild_id,reason,requested_at)
             VALUES (?1,'match_enrichment:9','2024-01-16 12:00:01')",
            [GUILD as i64],
        )
        .expect("insert durable replay marker");
}

#[test]
fn match_view_routes_round_trip_and_reject_extra_segments() {
    let route = MatchViewRoute::parse("enrichment:match:42:7:1234:1").unwrap();
    assert_eq!(
        route,
        MatchViewRoute {
            guild_id: 42,
            match_id: 7,
            expires_at: 1_234,
            page: 1,
        }
    );
    assert!(MatchViewRoute::parse("enrichment:match:42:7:1234:2").is_none());
    assert!(MatchViewRoute::parse("enrichment:match:42:7:1234:1:extra").is_none());
}

#[test]
fn registration_exposes_the_complete_python_tree() {
    let enrich = enrich_subcommands();
    assert_eq!(
        enrich
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        [
            "setleague",
            "match",
            "backfill",
            "config",
            "discover",
            "wipeall",
            "wipematch",
            "rebuildpairings",
        ]
    );
    assert_eq!(
        matches_subcommands()
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        ["history", "view", "recent"]
    );
}

#[test]
fn advantage_projection_preserves_integer_and_float_series() {
    let value = EnrichmentJsonValue::Object(BTreeMap::from([
        (
            "radiant_gold_adv".to_owned(),
            EnrichmentJsonValue::Array(vec![
                EnrichmentJsonValue::Integer(-10),
                EnrichmentJsonValue::Float(20.5),
            ]),
        ),
        (
            "radiant_xp_adv".to_owned(),
            EnrichmentJsonValue::Array(vec![EnrichmentJsonValue::Integer(30)]),
        ),
    ]));
    assert_eq!(
        advantage_data(&value),
        AdvantageData {
            radiant_gold: vec![-10.0, 20.5],
            radiant_xp: vec![30.0],
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_match_view_uses_exact_timeout_and_disables_both_controls() {
    assert_eq!(MATCH_VIEW_TIMEOUT, Duration::from_secs(120));
    let (_directory, path) = migrated();
    let catalog = dotabase();
    seed_enriched_view_match(&path);
    let server = RouteServer::start(Vec::new());
    let shared_services = services(&server);

    let provider = EnrichmentRegistrationProvider::with_dotabase_path(
        &path,
        &application_config(),
        Arc::clone(&shared_services),
        catalog.path(),
    )
    .expect("compose production timeout provider");
    let production_registry = registry(&provider);
    let responder = Arc::new(CapturingResponder::default());
    let before = unix_now();
    production_registry
        .command_handler("matches")
        .expect("matches command handler")
        .handle(
            command(
                "matches",
                "view",
                vec![
                    option("match_id", InteractionValue::Integer(7)),
                    option(
                        "user",
                        InteractionValue::User {
                            id: 101,
                            display_name: Some("Selected Player".to_owned()),
                            is_bot: Some(false),
                        },
                    ),
                ],
                100,
                Some(GUILD),
                None,
            ),
            responder.clone(),
        )
        .await
        .expect("production match view");
    let after = unix_now();
    {
        let captured = responder.captured.lock().expect("response capture");
        let response = captured.followups.last().expect("match view followup");
        assert!(
            response.embeds[0]
                .title
                .as_deref()
                .is_some_and(|title| title.starts_with("👑 Match #7"))
        );
        assert_eq!(
            response.embeds[0].thumbnail_url.as_deref(),
            Some(
                "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/antimage.png"
            ),
            "the invoking user's hero overrides the selected user's MVP hero"
        );
        let route = MatchViewRoute::parse(&response.components[0].buttons[1].custom_id)
            .expect("production match view route");
        assert!(route.expires_at >= before + 120);
        assert!(route.expires_at <= after + 120);
    }

    let short_provider = EnrichmentRegistrationProvider::compose(
        &path,
        &application_config(),
        shared_services,
        catalog.path(),
        Duration::from_millis(10),
    )
    .expect("compose accelerated timeout provider");
    let short_registry = registry(&short_provider);
    let receipt = InteractionMessageReceipt {
        message_id: 55,
        channel_id: 7,
        delivery: InteractionMessageDelivery::InteractionFollowup,
    };
    let timeout_responder = Arc::new(CapturingResponder {
        receipt: StdMutex::new(Some(receipt)),
        ..CapturingResponder::default()
    });
    short_registry
        .command_handler("matches")
        .expect("matches command handler")
        .handle(
            command(
                "matches",
                "view",
                vec![option("match_id", InteractionValue::Integer(7))],
                100,
                Some(GUILD),
                None,
            ),
            timeout_responder.clone(),
        )
        .await
        .expect("accelerated match view");
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if !timeout_responder
            .captured
            .lock()
            .expect("response capture")
            .message_edits
            .is_empty()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "match controls were not disabled"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let captured = timeout_responder.captured.lock().expect("response capture");
    let (edited_receipt, edit) = &captured.message_edits[0];
    assert_eq!(*edited_receipt, receipt);
    assert!(edit.content.is_empty());
    assert!(edit.embeds.is_empty());
    assert!(edit.attachments.is_empty());
    assert_eq!(edit.components.len(), 1);
    assert!(
        edit.components[0]
            .buttons
            .iter()
            .all(|button| button.disabled)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_match_view_uploads_advantage_attachment_on_component_edit() {
    let (_directory, path) = migrated();
    let catalog = dotabase();
    seed_enriched_view_match(&path);
    let server = RouteServer::start(Vec::new());
    let shared_services = services(&server);
    let provider = EnrichmentRegistrationProvider::with_dotabase_path(
        &path,
        &application_config(),
        Arc::clone(&shared_services),
        catalog.path(),
    )
    .expect("compose production match view provider");
    let production_registry = registry(&provider);
    let initial_responder = Arc::new(CapturingResponder::default());

    production_registry
        .command_handler("matches")
        .expect("matches command handler")
        .handle(
            command(
                "matches",
                "view",
                vec![
                    option("match_id", InteractionValue::Integer(7)),
                    option(
                        "user",
                        InteractionValue::User {
                            id: 101,
                            display_name: Some("Selected Player".to_owned()),
                            is_bot: Some(false),
                        },
                    ),
                ],
                100,
                Some(GUILD),
                None,
            ),
            initial_responder.clone(),
        )
        .await
        .expect("production match view response");

    let graph_custom_id = {
        let captured = initial_responder.captured.lock().expect("response capture");
        let response = captured.followups.last().expect("match view followup");
        assert_eq!(response.components.len(), 1);
        response.components[0].buttons[1].custom_id.clone()
    };

    let restarted = EnrichmentRegistrationProvider::with_dotabase_path(
        &path,
        &application_config(),
        Arc::clone(&shared_services),
        catalog.path(),
    )
    .expect("restart production match view provider");
    let restarted_registry = registry(&restarted);
    let component_responder = Arc::new(CapturingResponder::default());
    restarted_registry
        .component_handler(&graph_custom_id)
        .expect("restarted match graph component handler")
        .handle(
            InteractionRequest::Component {
                interaction_id: 10,
                custom_id: graph_custom_id,
                user_id: 999,
                user_display_name: "Any Clicker".to_owned(),
                guild_id: Some(GUILD),
                channel_id: Some(7),
                member_permissions: None,
                values: Vec::new(),
            },
            component_responder.clone(),
        )
        .await
        .expect("production advantage graph component");

    let captured = component_responder
        .captured
        .lock()
        .expect("component response capture");
    assert_eq!(captured.updates.len(), 1);
    let update = &captured.updates[0];
    assert_eq!(update.attachments.len(), 1);
    assert_eq!(update.attachments[0].filename, "advantage.png");
    assert!(!update.attachments[0].bytes.is_empty());
    assert_eq!(update.embeds.len(), 1);
    assert_eq!(
        update.embeds[0].image_url.as_deref(),
        Some("attachment://advantage.png")
    );
    assert!(!update.components[0].buttons[0].disabled);
    assert!(update.components[0].buttons[1].disabled);
}

async fn assert_production_manual_enrichment_is_atomic_and_match_view_survives_restart() {
    let (_directory, path) = migrated();
    let catalog = dotabase();
    seed_manual_match(&path);
    complete_manual_roster(&path);
    let server = RouteServer::start(vec![strict_manual_match_payload()]);
    let shared_services = services(&server);
    let provider = EnrichmentRegistrationProvider::with_dotabase_path(
        &path,
        &application_config(),
        Arc::clone(&shared_services),
        catalog.path(),
    )
    .expect("compose enrichment provider");
    let first_registry = registry(&provider);
    let responder = Arc::new(CapturingResponder::default());
    first_registry
        .command_handler("enrich")
        .expect("enrich command handler")
        .handle(
            command(
                "enrich",
                "match",
                vec![option("valve_match_id", InteractionValue::Integer(9_001))],
                ADMIN,
                Some(GUILD),
                None,
            ),
            responder.clone(),
        )
        .await
        .expect("manual enrichment response");

    {
        let captured = responder.captured.lock().expect("response capture");
        assert_eq!(captured.deferred, [true]);
        let response = captured.followups.last().expect("enrichment followup");
        assert!(response.ephemeral);
        assert_eq!(response.content, "Enriched 10/10 players");
        assert_eq!(response.embeds.len(), 1);
        assert_eq!(response.embeds[0].color, Some(DISCORD_GREEN));
        assert_eq!(
            response.embeds[0].thumbnail_url.as_deref(),
            Some(
                "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/antimage.png"
            )
        );
    }
    assert_eq!(server.requests(1), ["GET /matches/9001 HTTP/1.1"]);

    let connection = Connection::open(&path).expect("inspect enrichment database");
    let persisted_match: (i64, i64, i64, i64, i64, String) = connection
        .query_row(
            "SELECT valve_match_id,duration_seconds,radiant_score,dire_score,game_mode,
                    enrichment_source
             FROM matches WHERE match_id=7 AND guild_id=?1",
            [GUILD as i64],
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
        .expect("persisted enrichment");
    assert_eq!(
        persisted_match,
        (9_001, 2_400, 35, 22, 2, "manual".to_owned())
    );
    let participant: (i64, i64, i64, i64, i64, i64, f64) = connection
        .query_row(
            "SELECT hero_id,kills,deaths,assists,gpm,net_worth,fantasy_points
             FROM match_participants WHERE match_id=7 AND discord_id=100 AND guild_id=?1",
            [GUILD as i64],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .expect("persisted participant enrichment");
    assert_eq!(
        (
            participant.0,
            participant.1,
            participant.2,
            participant.3,
            participant.4,
            participant.5,
        ),
        (1, 10, 2, 5, 600, 20_000)
    );
    assert!(participant.6 > 0.0);
    let wrapped: (i64, i64, i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT actions_per_min,courier_kills,pings,rapier_count,lane_role,comeback,throw
             FROM wrapped_enrichment_facts
             WHERE guild_id=?1 AND match_id=7 AND discord_id=100",
            [GUILD as i64],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .expect("persisted Wrapped projection");
    assert_eq!(wrapped, (200, 1, 12, 1, 2, 123, 456));
    let bans = connection
        .prepare(
            "SELECT ban_index,team,hero_id FROM match_bans
             WHERE match_id=7 ORDER BY ban_index",
        )
        .and_then(|mut statement| {
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .expect("persisted Scout ban projection");
    assert_eq!(bans, [(0, 0, 44), (2, 1, 66)]);
    let payload: String = connection
        .query_row(
            "SELECT enrichment_data FROM matches WHERE match_id=7",
            [],
            |row| row.get(0),
        )
        .expect("persisted raw OpenDota payload");
    assert!(payload.contains("radiant_gold_adv"));
    drop(connection);

    let history_responder = Arc::new(CapturingResponder::default());
    first_registry
        .command_handler("matches")
        .expect("matches command handler")
        .handle(
            command(
                "matches",
                "history",
                vec![option("limit", InteractionValue::Integer(1))],
                100,
                Some(GUILD),
                None,
            ),
            history_responder.clone(),
        )
        .await
        .expect("match history response");
    {
        let captured = history_responder
            .captured
            .lock()
            .expect("history response capture");
        assert_eq!(captured.deferred, [true]);
        assert_eq!(
            captured.followups[0].embeds[0].thumbnail_url.as_deref(),
            Some(
                "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/antimage.png"
            )
        );
    }

    let view_responder = Arc::new(CapturingResponder::default());
    first_registry
        .command_handler("matches")
        .expect("matches command handler")
        .handle(
            command(
                "matches",
                "view",
                vec![option("match_id", InteractionValue::Integer(7))],
                100,
                Some(GUILD),
                None,
            ),
            view_responder.clone(),
        )
        .await
        .expect("match view response");
    let graph_custom_id = {
        let captured = view_responder.captured.lock().expect("response capture");
        assert_eq!(captured.deferred, [false]);
        let response = captured.followups.last().expect("match view followup");
        assert!(!response.ephemeral);
        assert_eq!(response.components.len(), 1);
        response.components[0].buttons[1].custom_id.clone()
    };

    let restarted = EnrichmentRegistrationProvider::with_dotabase_path(
        &path,
        &application_config(),
        Arc::clone(&shared_services),
        catalog.path(),
    )
    .expect("restart enrichment provider");
    let restarted_registry = registry(&restarted);
    let component_responder = Arc::new(CapturingResponder::default());
    restarted_registry
        .component_handler(&graph_custom_id)
        .expect("restarted match component handler")
        .handle(
            InteractionRequest::Component {
                interaction_id: 10,
                custom_id: graph_custom_id,
                user_id: 999,
                user_display_name: "Any Clicker".to_owned(),
                guild_id: Some(GUILD),
                channel_id: Some(7),
                member_permissions: None,
                values: Vec::new(),
            },
            component_responder.clone(),
        )
        .await
        .expect("restarted graph component");
    let captured = component_responder
        .captured
        .lock()
        .expect("component response capture");
    assert_eq!(captured.updates.len(), 1);
    assert_eq!(captured.updates[0].attachments[0].filename, "advantage.png");
    assert!(!captured.updates[0].attachments[0].bytes.is_empty());
    assert_eq!(
        captured.updates[0].embeds[0].image_url.as_deref(),
        Some("attachment://advantage.png")
    );
    assert!(captured.updates[0].components[0].buttons[1].disabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_manual_enrichment_is_atomic_and_match_view_survives_restart() {
    assert_production_manual_enrichment_is_atomic_and_match_view_survives_restart().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_enrichment_write_derives_compact_facts() {
    assert_production_manual_enrichment_is_atomic_and_match_view_survives_restart().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_enrichment_force_flag_relaxes_validation_and_default_stays_strict() {
    let (_directory, path) = migrated();
    let catalog = dotabase();
    // Only 1 of 10 lobby players is linked and present in the OpenDota
    // payload, so strict validation can never pass for this match.
    seed_manual_match(&path);
    let server = RouteServer::start(vec![manual_match_payload(), manual_match_payload()]);
    let provider = EnrichmentRegistrationProvider::with_dotabase_path(
        &path,
        &application_config(),
        services(&server),
        catalog.path(),
    )
    .expect("compose enrichment provider");
    let registry = registry(&provider);

    // Without force (default), the service keeps strict roster validation.
    let strict_responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("enrich")
        .expect("enrich command handler")
        .handle(
            command(
                "enrich",
                "match",
                vec![option("valve_match_id", InteractionValue::Integer(9_001))],
                ADMIN,
                Some(GUILD),
                None,
            ),
            strict_responder.clone(),
        )
        .await
        .expect("strict manual enrichment response");
    {
        let captured = strict_responder.captured.lock().expect("response capture");
        assert_eq!(captured.deferred, [true]);
        let response = captured.followups.last().expect("strict followup");
        assert!(response.ephemeral);
        assert!(response.content.starts_with("Enrichment failed:"));
        assert!(response.content.contains("(need 10)"));
        assert!(response.content.contains("force:True"));
    }
    let connection = Connection::open(&path).expect("inspect strict refusal");
    let untouched: Option<i64> = connection
        .query_row(
            "SELECT valve_match_id FROM matches WHERE match_id=7 AND guild_id=?1",
            [GUILD as i64],
            |row| row.get(0),
        )
        .expect("strict refusal leaves the match unlinked");
    assert_eq!(untouched, None);
    drop(connection);

    // force:true reaches the service with validation relaxed and enriches
    // the same explicit Valve ID.
    let force_responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("enrich")
        .expect("enrich command handler")
        .handle(
            command(
                "enrich",
                "match",
                vec![
                    option("valve_match_id", InteractionValue::Integer(9_001)),
                    option("force", InteractionValue::Boolean(true)),
                ],
                ADMIN,
                Some(GUILD),
                None,
            ),
            force_responder.clone(),
        )
        .await
        .expect("forced manual enrichment response");
    {
        let captured = force_responder.captured.lock().expect("response capture");
        assert_eq!(captured.deferred, [true]);
        let response = captured.followups.last().expect("forced followup");
        assert!(response.ephemeral);
        assert_eq!(response.content, "Enriched 1/10 players");
    }
    let connection = Connection::open(&path).expect("inspect forced enrichment");
    let persisted: (i64, String) = connection
        .query_row(
            "SELECT valve_match_id,enrichment_source FROM matches
             WHERE match_id=7 AND guild_id=?1",
            [GUILD as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("forced enrichment persisted");
    assert_eq!(persisted, (9_001, "manual".to_owned()));
    assert_eq!(
        server.requests(2),
        [
            "GET /matches/9001 HTTP/1.1".to_owned(),
            "GET /matches/9001 HTTP/1.1".to_owned(),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_discovery_reuses_one_detail_fetch_and_persists_auto_provenance() {
    let (_directory, path) = migrated();
    seed_discovery_match(&path);
    let history = serde_json::json!([{
        "match_id": 9_002,
        "start_time": 1_705_317_900_i64
    }])
    .to_string();
    let server = RouteServer::start(vec![history, discovery_match_payload()]);
    let provider =
        EnrichmentRegistrationProvider::new(&path, &application_config(), services(&server))
            .expect("compose discovery provider");
    let registry = registry(&provider);
    let responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("enrich")
        .expect("enrich command handler")
        .handle(
            command(
                "enrich",
                "discover",
                vec![option("dry_run", InteractionValue::Boolean(false))],
                ADMIN,
                Some(GUILD),
                None,
            ),
            responder.clone(),
        )
        .await
        .expect("discovery response");

    let captured = responder.captured.lock().expect("response capture");
    assert_eq!(captured.deferred, [true]);
    assert_eq!(captured.followups.len(), 1);
    assert_eq!(
        captured.followups[0].content,
        "Starting match discovery (dry_run=false)... This may take a while."
    );
    assert!(captured.followups[0].ephemeral);
    assert_eq!(captured.original_edits.len(), 1);
    assert!(captured.original_edits[0].ephemeral);
    assert!(captured.original_edits[0].content.contains("Discovered: 1"));
    assert!(
        captured.original_edits[0]
            .content
            .contains("#8 → 9002 (100% - 10/10 players)")
    );
    drop(captured);

    let requests = server.requests(2);
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /players/20000/matches?"));
    assert!(requests[0].contains("limit=500"));
    assert!(requests[0].contains("significant=0"));
    assert!(requests[0].contains("project=match_id"));
    assert!(requests[0].contains("project=start_time"));
    assert_eq!(requests[1], "GET /matches/9002 HTTP/1.1");

    let connection = Connection::open(&path).expect("inspect discovery database");
    let persisted: (i64, String, f64, i64) = connection
        .query_row(
            "SELECT valve_match_id,enrichment_source,enrichment_confidence,duration_seconds
             FROM matches WHERE match_id=8 AND guild_id=?1",
            [GUILD as i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("persisted auto discovery");
    assert_eq!(persisted, (9_002, "auto".to_owned(), 1.0, 2_100));
    let enriched: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM match_participants
             WHERE match_id=8 AND guild_id=?1 AND fantasy_points IS NOT NULL",
            [GUILD as i64],
            |row| row.get(0),
        )
        .expect("count discovered participants");
    assert_eq!(enriched, 10);
    let wrapped: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM wrapped_enrichment_facts
             WHERE match_id=8 AND guild_id=?1",
            [GUILD as i64],
            |row| row.get(0),
        )
        .expect("count discovered Wrapped facts");
    assert_eq!(wrapped, 10);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_stops_at_local_daily_quota_before_http_or_database_work() {
    let (_directory, path) = migrated();
    let server = RouteServer::start(Vec::new());
    let mut quota_config = OpenDotaHttpConfig::new(&server.base_url, None);
    quota_config.requests_per_day = Some(0);
    quota_config.rate_limit_wait = Duration::from_millis(1);
    let quota_services = Arc::new(
        OpenDotaRuntimeServices::with_config(quota_config).expect("quota-limited services"),
    );
    let provider =
        EnrichmentRegistrationProvider::new(&path, &application_config(), quota_services)
            .expect("compose quota-limited provider");
    let responder = Arc::new(CapturingResponder::default());

    registry(&provider)
        .command_handler("enrich")
        .expect("enrich command handler")
        .handle(
            command(
                "enrich",
                "discover",
                vec![option("dry_run", InteractionValue::Boolean(false))],
                ADMIN,
                Some(GUILD),
                None,
            ),
            responder.clone(),
        )
        .await
        .expect("quota stop response");

    let captured = responder.captured.lock().expect("response capture");
    assert_eq!(captured.deferred, [true]);
    assert_eq!(captured.followups.len(), 1);
    assert!(captured.followups[0].content.contains("quota protection"));
    assert!(captured.followups[0].content.contains("Daily: 0/0"));
    assert!(captured.original_edits.is_empty());
    assert!(server.requests(0).is_empty());
}

#[test]
fn production_constructor_recovers_durable_openskill_replay_after_restart() {
    let (_directory, path) = migrated();
    seed_pending_openskill_replay(&path);

    let _provider =
        EnrichmentRegistrationProvider::new(&path, &application_config(), offline_services())
            .expect("startup recovers pending OpenSkill replay");

    let connection = Connection::open(&path).expect("inspect recovered OpenSkill state");
    let pending: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM openskill_replay_jobs WHERE guild_id=?1",
            [GUILD as i64],
            |row| row.get(0),
        )
        .expect("pending replay count");
    assert_eq!(pending, 0);
    let history: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM rating_history
             WHERE guild_id=?1 AND match_id=9
               AND os_mu_before IS NOT NULL AND os_mu_after IS NOT NULL
               AND fantasy_weight IS NOT NULL",
            [GUILD as i64],
            |row| row.get(0),
        )
        .expect("replayed history count");
    assert_eq!(history, 10);
    let rated_players: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM players
             WHERE guild_id=?1 AND os_mu IS NOT NULL AND os_sigma IS NOT NULL",
            [GUILD as i64],
            |row| row.get(0),
        )
        .expect("replayed player count");
    assert_eq!(rated_players, 10);
    let prediction: (f64, f64, i64, String) = connection
        .query_row(
            "SELECT openskill_radiant_win_prob,openskill_raw_radiant_win_prob,
                    openskill_algorithm_version,openskill_algorithm_fingerprint
             FROM match_predictions WHERE match_id=9",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("replayed match prediction");
    assert!((0.0..=1.0).contains(&prediction.0));
    assert!((0.0..=1.0).contains(&prediction.1));
    assert!(prediction.2 > 0);
    assert!(!prediction.3.is_empty());
    let revision: i64 = connection
        .query_row(
            "SELECT revision FROM openskill_rating_revisions WHERE guild_id=?1",
            [GUILD as i64],
            |row| row.get(0),
        )
        .expect("OpenSkill revision");
    assert_eq!(revision, 1);
}

#[test]
fn phase_two_skips_until_all_ten_fantasy_points_exist() {
    let (_directory, path) = migrated();
    seed_pending_openskill_replay(&path);
    let connection = Connection::open(&path).expect("open no-fantasy replay fixture");
    connection
        .execute(
            "UPDATE match_participants SET fantasy_points=NULL
             WHERE match_id=9 AND guild_id=?1",
            [GUILD as i64],
        )
        .expect("remove fantasy points from replay fixture");
    connection
        .execute(
            "DELETE FROM openskill_replay_jobs WHERE guild_id=?1",
            [GUILD as i64],
        )
        .expect("remove unrelated pending replay marker");

    let replay = ConfiguredOpenSkillReplay {
        rating_system: cama_domain::rating::CamaRatingSystem::default(),
        completion: MatchDiscoveryRepository::new(&path),
        correction: MatchCorrectionRepository::new(&path),
        system: CamaOpenSkillSystem::new(),
        new_player_mmr_discount: 500,
    };
    let result = replay
        .update_openskill_ratings_for_match(
            InternalMatchId(9),
            Some(cama_app::dedicated_lobby_channel::GuildId(GUILD as i64)),
        )
        .expect("missing fantasy is a successful no-op");
    assert_eq!(
        result,
        OpenSkillEnrichmentResult {
            success: true,
            players_updated: 0,
            error: None,
        }
    );
    let untouched: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM players
             WHERE guild_id=?1 AND os_mu IS NOT NULL",
            [GUILD as i64],
            |row| row.get(0),
        )
        .expect("count untouched OpenSkill players");
    assert_eq!(untouched, 0);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM openskill_replay_jobs WHERE guild_id=?1",
                [GUILD as i64],
                |row| row.get::<_, i64>(0),
            )
            .expect("count replay markers"),
        0
    );
}

#[test]
fn production_constructor_leaves_match_correction_replay_for_admin_recovery() {
    let (_directory, path) = migrated();
    seed_pending_openskill_replay(&path);
    let connection = Connection::open(&path).expect("open correction recovery fixture");
    connection
        .execute(
            "UPDATE openskill_replay_jobs SET reason='match_correction:9'
             WHERE guild_id=?1",
            [GUILD as i64],
        )
        .expect("mark replay as correction-owned");
    drop(connection);

    let _provider =
        EnrichmentRegistrationProvider::new(&path, &application_config(), offline_services())
            .expect("compose while preserving correction recovery marker");

    let connection = Connection::open(&path).expect("inspect preserved correction job");
    let pending: (String, i64, i64) = connection
        .query_row(
            "SELECT reason,
                    (SELECT COUNT(*) FROM rating_history
                     WHERE guild_id=?1 AND match_id=9),
                    (SELECT COUNT(*) FROM openskill_rating_revisions WHERE guild_id=?1)
             FROM openskill_replay_jobs WHERE guild_id=?1",
            [GUILD as i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("load untouched correction replay marker");
    assert_eq!(pending, ("match_correction:9".to_owned(), 0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_permissions_visibility_backfill_recent_and_atomic_wipe_match_python() {
    let (_directory, path) = migrated();
    seed_manual_match(&path);
    complete_manual_roster(&path);
    let connection = Connection::open(&path).expect("open enrichment fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username,dotabuff_url)
             VALUES (200,?1,'Backfill','https://www.dotabuff.com/players/76561197960278049')",
            [GUILD as i64],
        )
        .expect("insert backfill player");
    drop(connection);
    let recent_payload = r#"[{
        "match_id":44,"player_slot":0,"radiant_win":true,"hero_id":999,
        "kills":7,"deaths":3,"assists":11,"duration":1800
    }]"#
    .to_owned();
    let server = RouteServer::start(vec![strict_manual_match_payload(), recent_payload]);
    let provider =
        EnrichmentRegistrationProvider::new(&path, &application_config(), services(&server))
            .expect("compose enrichment provider");
    let registry = registry(&provider);

    let unauthorized = Arc::new(CapturingResponder::default());
    registry
        .command_handler("enrich")
        .expect("enrich command handler")
        .handle(
            command("enrich", "wipeall", Vec::new(), 99, Some(GUILD), None),
            unauthorized.clone(),
        )
        .await
        .expect("unauthorized response");
    {
        let captured = unauthorized.captured.lock().expect("response capture");
        assert!(captured.deferred.is_empty());
        assert_eq!(
            captured.immediate,
            [InteractionResponse::message(ADMIN_ONLY).ephemeral()]
        );
    }

    let dm = Arc::new(CapturingResponder::default());
    registry
        .command_handler("matches")
        .expect("matches command handler")
        .handle(
            command("matches", "history", Vec::new(), 100, None, None),
            dm.clone(),
        )
        .await
        .expect("guild gate response");
    assert_eq!(
        dm.captured.lock().expect("response capture").immediate,
        [InteractionResponse::message(GUILD_ONLY).ephemeral()]
    );

    let enrich = Arc::new(CapturingResponder::default());
    registry
        .command_handler("enrich")
        .expect("enrich command handler")
        .handle(
            command(
                "enrich",
                "match",
                vec![option("valve_match_id", InteractionValue::Integer(9_001))],
                ADMIN,
                Some(GUILD),
                None,
            ),
            enrich,
        )
        .await
        .expect("manual enrichment response");

    let backfill = Arc::new(CapturingResponder::default());
    registry
        .command_handler("enrich")
        .expect("enrich command handler")
        .handle(
            command("enrich", "backfill", Vec::new(), ADMIN, None, None),
            backfill.clone(),
        )
        .await
        .expect("Dotabuff backfill response");
    {
        let captured = backfill.captured.lock().expect("response capture");
        assert_eq!(captured.deferred, [true]);
        assert!(captured.followups[0].ephemeral);
        assert!(
            captured.followups[0]
                .content
                .contains("Steam ID backfill complete")
        );
    }
    let connection = Connection::open(&path).expect("inspect backfill");
    let steam_id: i64 = connection
        .query_row(
            "SELECT steam_id FROM player_steam_ids WHERE discord_id=200",
            [],
            |row| row.get(0),
        )
        .expect("backfilled Steam ID");
    assert_eq!(steam_id, 12_321);
    drop(connection);

    let recent = Arc::new(CapturingResponder::default());
    registry
        .command_handler("matches")
        .expect("matches command handler")
        .handle(
            command(
                "matches",
                "recent",
                vec![option("limit", InteractionValue::Integer(1))],
                100,
                None,
                None,
            ),
            recent.clone(),
        )
        .await
        .expect("recent matches response");
    {
        let captured = recent.captured.lock().expect("response capture");
        assert_eq!(captured.deferred, [false]);
        assert!(!captured.followups[0].ephemeral);
        assert_eq!(
            captured.followups[0].attachments[0].filename,
            "recent_matches.png"
        );
        assert!(!captured.followups[0].attachments[0].bytes.is_empty());
    }
    let requests = server.requests(2);
    assert!(requests.iter().any(|request| {
        request.starts_with("GET /players/12345/matches?") && request.contains("limit=1")
    }));

    let wipe = Arc::new(CapturingResponder::default());
    registry
        .command_handler("enrich")
        .expect("enrich command handler")
        .handle(
            command("enrich", "wipeall", Vec::new(), ADMIN, Some(GUILD), None),
            wipe.clone(),
        )
        .await
        .expect("wipe-all response");
    let captured = wipe.captured.lock().expect("response capture");
    assert_eq!(captured.deferred, [true]);
    assert!(captured.followups[0].ephemeral);
    assert!(
        captured.followups[0]
            .content
            .contains("Wiped 1 enrichments")
    );
    drop(captured);
    let connection = Connection::open(&path).expect("inspect wipe");
    let match_state: (Option<i64>, Option<String>) = connection
        .query_row(
            "SELECT valve_match_id,enrichment_data FROM matches WHERE match_id=7",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("wiped match");
    assert_eq!(match_state, (None, None));
    let hero: Option<i64> = connection
        .query_row(
            "SELECT hero_id FROM match_participants WHERE match_id=7 AND discord_id=100",
            [],
            |row| row.get(0),
        )
        .expect("wiped participant");
    assert_eq!(hero, None);
    let facts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM wrapped_enrichment_facts WHERE guild_id=?1 AND match_id=7",
            [GUILD as i64],
            |row| row.get(0),
        )
        .expect("wiped Wrapped facts");
    assert_eq!(facts, 0);
}

#[test]
fn recorded_match_discovery_status_policy_matches_python_retry_contract() {
    for status in [
        DiscoveryStatus::LowConfidence,
        DiscoveryStatus::NoCandidates,
        DiscoveryStatus::UpstreamUnavailable,
        DiscoveryStatus::Deferred,
        DiscoveryStatus::InProgress,
    ] {
        assert_eq!(
            recorded_discovery_disposition(status),
            RecordedDiscoveryDisposition::Retry
        );
    }
    assert_eq!(
        recorded_discovery_disposition(DiscoveryStatus::Discovered),
        RecordedDiscoveryDisposition::Publish
    );
    for status in [
        DiscoveryStatus::NoSteamIds,
        DiscoveryStatus::NoTimestamp,
        DiscoveryStatus::NotFound,
        DiscoveryStatus::ValidationFailed,
        DiscoveryStatus::Error,
    ] {
        assert_eq!(
            recorded_discovery_disposition(status),
            RecordedDiscoveryDisposition::Stop
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorded_match_discovery_retries_not_ready_then_returns_live_enriched_publication() {
    let (_directory, path) = migrated();
    seed_discovery_match(&path);
    let history = serde_json::json!([{
        "match_id": 9_002,
        "start_time": 1_705_317_900_i64
    }])
    .to_string();
    let mut bodies = vec!["[]".to_owned(); 5];
    bodies.push(history);
    bodies.push(discovery_match_payload());
    let server = RouteServer::start(bodies);
    let mut config = application_config();
    config.values.enrichment_retry_delays = vec![0, 0];
    let catalog = dotabase();
    let provider = EnrichmentRegistrationProvider::with_dotabase_path(
        &path,
        &config,
        services(&server),
        catalog.path(),
    )
    .expect("compose shared discovery provider");

    let outcome = provider
        .recorded_match_discovery()
        .discover_recorded_match(GUILD as i64, 8)
        .await
        .expect("post-record discovery");
    let RecordedMatchDiscoveryOutcome::Discovered { result, response } = outcome else {
        panic!("expected discovered result after retry");
    };
    assert_eq!(result.status, DiscoveryStatus::Discovered);
    assert_eq!(result.valve_match_id.map(|id| id.0), Some(9_002));
    assert_eq!(
        response.content,
        "📊 Match #8 auto-enriched (100% confidence)"
    );
    assert_eq!(response.embeds.len(), 1);
    assert!(!response.ephemeral);

    let requests = server.requests(7);
    assert_eq!(requests.len(), 7);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /players/"))
            .count(),
        6
    );
    assert_eq!(
        requests.last().map(String::as_str),
        Some("GET /matches/9002 HTTP/1.1")
    );
    let persisted: (i64, String) = Connection::open(&path)
        .expect("inspect recorded discovery")
        .query_row(
            "SELECT valve_match_id,enrichment_source
             FROM matches WHERE guild_id=?1 AND match_id=8",
            [GUILD as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("persisted post-record enrichment");
    assert_eq!(persisted, (9_002, "auto".to_owned()));
    let diagnostic: (String, String) = Connection::open(&path)
        .expect("inspect discovery diagnostic")
        .query_row(
            "SELECT status,diagnostics_json
             FROM match_discovery_attempts
             WHERE guild_id=?1 AND internal_match_id=8
             ORDER BY attempt_id DESC LIMIT 1",
            [GUILD as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("persisted discovery diagnostic");
    assert_eq!(diagnostic.0, "discovered");
    assert!(diagnostic.1.contains("validated"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recorded_match_discovery_honors_disabled_guild_without_http() {
    let (_directory, path) = migrated();
    seed_discovery_match(&path);
    GuildConfigRepository::new(&path, false)
        .set_auto_enrich(GUILD as i64, false)
        .expect("disable auto enrichment");
    let server = RouteServer::start(Vec::new());
    let catalog = dotabase();
    let provider = EnrichmentRegistrationProvider::with_dotabase_path(
        &path,
        &application_config(),
        services(&server),
        catalog.path(),
    )
    .expect("compose disabled discovery provider");
    let outcome = provider
        .recorded_match_discovery()
        .discover_recorded_match(GUILD as i64, 8)
        .await
        .expect("disabled post-record discovery");
    assert_eq!(outcome, RecordedMatchDiscoveryOutcome::Disabled);
    assert!(server.requests(0).is_empty());
}

#[tokio::test]
async fn refresh_parsed_acknowledges_before_doing_any_work() {
    // The sweep scans the database and makes network calls, so the
    // interaction must be deferred first. Returning before `defer` fails the
    // command outright with "This interaction failed", which is exactly what
    // an earlier revision of this branch did.
    let (_directory, path) = migrated();
    let catalog = dotabase();
    let server = RouteServer::start(vec![]);
    let shared_services = services(&server);
    let provider = EnrichmentRegistrationProvider::with_dotabase_path(
        &path,
        &application_config(),
        Arc::clone(&shared_services),
        catalog.path(),
    )
    .expect("compose enrichment provider");
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .command_handler("enrich")
        .expect("enrich command handler")
        .handle(
            command(
                "enrich",
                "discover",
                vec![
                    option("refresh_parsed", InteractionValue::Boolean(true)),
                    option("dry_run", InteractionValue::Boolean(true)),
                ],
                ADMIN,
                Some(GUILD),
                None,
            ),
            responder.clone(),
        )
        .await
        .expect("refresh parsed response");

    let captured = responder.captured.lock().expect("response capture");
    assert_eq!(captured.deferred, [true], "must defer before scanning");
    let response = captured.followups.last().expect("refresh followup");
    assert!(response.ephemeral, "admin output stays private");
    assert!(
        response
            .content
            .contains("No matches are missing parsed stats"),
        "unexpected content: {}",
        response.content
    );
}

/// Seed `count` matches that are linked to a Dota id but whose participants
/// have no lane or farm data - the shape left behind when enrichment stored a
/// payload before OpenDota finished parsing the replay.
fn seed_stale_parsed_matches(path: &std::path::Path, count: i64) {
    let connection = Connection::open(path).expect("open stale fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    connection
        .execute(
            "INSERT OR IGNORE INTO players(discord_id,guild_id,discord_username,steam_id)
             VALUES (?1,?2,'Radiant',12345)",
            params![100_i64, GUILD as i64],
        )
        .expect("insert player");
    for index in 0..count {
        let match_id = 1_000 + index;
        connection
            .execute(
                "INSERT INTO matches(
                     match_id,guild_id,team1_players,team2_players,winning_team,
                     match_date,valve_match_id,enrichment_source,enrichment_data
                 ) VALUES (?1,?2,'[100]','[]',1,'2024-01-15 12:00:00',?3,'auto','{}')",
                params![match_id, GUILD as i64, 9_000 + index],
            )
            .expect("insert stale match");
        connection
            .execute(
                "INSERT INTO match_participants(
                     match_id,discord_id,guild_id,team_number,won,side
                 ) VALUES (?1,100,?2,1,1,'radiant')",
                params![match_id, GUILD as i64],
            )
            .expect("insert stale participant");
    }
}

#[tokio::test]
async fn refresh_parsed_drains_a_backlog_larger_than_one_chunk() {
    // The fixed snapshot is reported in 25-match chunks. A backlog of 30
    // therefore verifies that every candidate from the initial snapshot is
    // processed, not just the first progress chunk.
    const BACKLOG: i64 = 30;
    let (_directory, path) = migrated();
    let catalog = dotabase();
    seed_stale_parsed_matches(&path, BACKLOG);
    let bodies = (0..BACKLOG)
        .map(|_| parsed_match_payload())
        .collect::<Vec<_>>();
    let server = RouteServer::start(bodies);
    let shared_services = services(&server);
    let provider = EnrichmentRegistrationProvider::with_dotabase_path(
        &path,
        &application_config(),
        Arc::clone(&shared_services),
        catalog.path(),
    )
    .expect("compose enrichment provider");
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .command_handler("enrich")
        .expect("enrich command handler")
        .handle(
            command(
                "enrich",
                "discover",
                vec![option("refresh_parsed", InteractionValue::Boolean(true))],
                ADMIN,
                Some(GUILD),
                None,
            ),
            responder.clone(),
        )
        .await
        .expect("refresh parsed response");

    let captured = responder.captured.lock().expect("response capture");
    assert_eq!(captured.deferred, [true]);
    let summary = captured.followups.last().expect("refresh summary");
    assert!(
        summary
            .content
            .contains(&format!("Matches processed: {BACKLOG}")),
        "the sweep must process past a single chunk, got: {}",
        summary.content
    );
    assert!(
        summary
            .content
            .contains(&format!("Position inputs complete: {BACKLOG}")),
        "complete parsed responses should be verified, got: {}",
        summary.content
    );

    // The queue is genuinely empty because all required parsed fields landed,
    // not merely because an attempt counter hid the rows.
    let remaining = MatchRepository::new(&path)
        .matches_missing_parsed_stats(Some(GUILD as i64), 100)
        .expect("remaining candidates");
    assert!(
        remaining.is_empty(),
        "every match should be accounted for, {} left",
        remaining.len()
    );
}

#[tokio::test]
async fn refresh_parsed_keeps_partial_responses_eligible_for_a_later_run() {
    let (_directory, path) = migrated();
    let catalog = dotabase();
    seed_stale_parsed_matches(&path, 1);
    let server = RouteServer::start(vec![manual_match_payload(), parsed_match_payload()]);
    let shared_services = services(&server);
    let provider = EnrichmentRegistrationProvider::with_dotabase_path(
        &path,
        &application_config(),
        Arc::clone(&shared_services),
        catalog.path(),
    )
    .expect("compose enrichment provider");
    let responder = Arc::new(CapturingResponder::default());
    let handler = registry(&provider)
        .command_handler("enrich")
        .expect("enrich command handler");

    handler
        .handle(
            command(
                "enrich",
                "discover",
                vec![option("refresh_parsed", InteractionValue::Boolean(true))],
                ADMIN,
                Some(GUILD),
                None,
            ),
            responder.clone(),
        )
        .await
        .expect("first parsed refresh");

    {
        let captured = responder.captured.lock().expect("response capture");
        let summary = captured.followups.last().expect("first refresh summary");
        assert!(
            summary.content.contains("Still incomplete: 1"),
            "a successful but unparsed response must stay visible: {}",
            summary.content
        );
    }
    let repository = MatchRepository::new(&path);
    assert_eq!(
        repository
            .matches_missing_parsed_stats(Some(GUILD as i64), 100)
            .expect("partial candidates"),
        vec![(1_000, 9_000)],
        "an attempted partial response must remain eligible"
    );
    let attempts: i64 = Connection::open(&path)
        .expect("inspect attempts")
        .query_row(
            "SELECT parsed_refresh_attempts FROM matches WHERE guild_id=?1 AND match_id=1000",
            [GUILD as i64],
            |row| row.get(0),
        )
        .expect("attempt count");
    assert_eq!(attempts, 1);

    handler
        .handle(
            command(
                "enrich",
                "discover",
                vec![option("refresh_parsed", InteractionValue::Boolean(true))],
                ADMIN,
                Some(GUILD),
                None,
            ),
            responder.clone(),
        )
        .await
        .expect("second parsed refresh");

    let captured = responder.captured.lock().expect("response capture");
    let summary = captured.followups.last().expect("second refresh summary");
    assert!(
        summary.content.contains("Position inputs complete: 1"),
        "a later parsed response should clear the candidate: {}",
        summary.content
    );
    assert!(
        repository
            .matches_missing_parsed_stats(Some(GUILD as i64), 100)
            .expect("completed candidates")
            .is_empty()
    );
}
