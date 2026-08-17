use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crate::test_support::initialize_test_database as initialize_or_migrate;
use rusqlite::{Connection, params};
use tempfile::TempDir;

use super::*;
use crate::registration::{InteractionMessageDelivery, InteractionResponseError, Registry};

const GUILD: i64 = 42;
const USER: i64 = 100;

fn portrait_png() -> Vec<u8> {
    NativeScoutImageRenderer::default()
        .render(&ScoutReportInput {
            scout_data: ScoutData {
                player_count: 0,
                total_matches: Some(0),
                heroes: Vec::new(),
            },
            player_names: Vec::new(),
            title: "portrait fixture".to_owned(),
        })
        .expect("render portrait fixture")
}

fn scout_hero(hero_id: i64) -> cama_app::scout::ScoutHero {
    cama_app::scout::ScoutHero {
        hero_id,
        games: 1,
        wins: 1,
        losses: 0,
        bans: 0,
        primary_role: 1,
    }
}

struct PortraitServer {
    base_url: String,
    paths: Arc<Mutex<Vec<String>>>,
    max_active: Arc<AtomicUsize>,
}

impl PortraitServer {
    fn start(expected: usize, payload: Vec<u8>, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind portrait server");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("portrait address")
        );
        let paths = Arc::new(Mutex::new(Vec::new()));
        let captured_paths = Arc::clone(&paths);
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let captured_max = Arc::clone(&max_active);
        thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..expected {
                let (mut stream, _) = listener.accept().expect("accept portrait request");
                let payload = payload.clone();
                let paths = Arc::clone(&captured_paths);
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&captured_max);
                workers.push(thread::spawn(move || {
                    let mut request = [0_u8; 2_048];
                    let read = stream.read(&mut request).expect("read portrait request");
                    let line = String::from_utf8_lossy(&request[..read])
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    paths.lock().expect("portrait paths").push(line);
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(current, Ordering::SeqCst);
                    thread::sleep(delay);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        payload.len()
                    );
                    stream.write_all(response.as_bytes()).expect("write headers");
                    stream.write_all(&payload).expect("write portrait");
                    active.fetch_sub(1, Ordering::SeqCst);
                }));
            }
            for worker in workers {
                worker.join().expect("portrait worker");
            }
        });
        Self {
            base_url,
            paths,
            max_active,
        }
    }
}

struct Fixture {
    _directory: TempDir,
    database: PathBuf,
    dotabase: PathBuf,
    cache: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary Scout fixture");
        let database = directory.path().join("cama.db");
        let dotabase = directory.path().join("dotabase.db");
        let cache = directory.path().join("cache");
        initialize_or_migrate(&database).expect("migrate Scout fixture");
        Connection::open(&dotabase)
            .expect("open Dotabase fixture")
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
                     base_movement REAL,
                     base_armor REAL,
                     attack_rate REAL,
                     attr_strength_base REAL,
                     attr_agility_base REAL,
                     attr_intelligence_base REAL,
                     attr_strength_gain REAL,
                     attr_agility_gain REAL,
                     attr_intelligence_gain REAL,
                     vision_night REAL,
                     turn_rate REAL,
                     attack_damage_min REAL,
                     attack_damage_max REAL
                 );
                 INSERT INTO heroes (id, name, localized_name)
                 VALUES (1, NULL, 'Anti-Mage');",
            )
            .expect("create Dotabase fixture schema");
        Self {
            _directory: directory,
            database,
            dotabase,
            cache,
        }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(&self.database).expect("open Scout database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match production SQLite policy");
        connection
    }

    fn register(&self, discord_id: i64, name: &str, steam_id: Option<i64>) {
        self.connection()
            .execute(
                "INSERT INTO players (discord_id,guild_id,discord_username,steam_id)
                 VALUES (?1,?2,?3,?4)",
                params![discord_id, GUILD, name, steam_id],
            )
            .expect("insert Scout player");
    }

    fn seed_hero_rows(&self, count: i64) {
        let mut connection = self.connection();
        let transaction = connection.transaction().expect("Scout transaction");
        for hero_id in 1..=count {
            transaction
                .execute(
                    "INSERT INTO matches
                        (match_id,guild_id,team1_players,team2_players,winning_team)
                     VALUES (?1,?2,'[100]','[200]',1)",
                    params![hero_id, GUILD],
                )
                .expect("insert Scout match");
            transaction
                .execute(
                    "INSERT INTO match_participants
                        (match_id,discord_id,guild_id,team_number,won,hero_id,lane_role)
                     VALUES (?1,?2,?3,1,?4,?5,1)",
                    params![hero_id, USER, GUILD, i64::from(hero_id % 2 == 0), hero_id],
                )
                .expect("insert Scout participant");
        }
        transaction.commit().expect("commit Scout fixture");
    }

    fn cache_portrait(&self, hero_id: i64, bytes: &[u8]) {
        let path = self.cache.join("heroes").join(format!("{hero_id}.png"));
        std::fs::create_dir_all(path.parent().expect("portrait cache parent"))
            .expect("create portrait cache");
        std::fs::write(path, bytes).expect("cache portrait PNG");
    }

    fn provider(&self, timeout: Duration) -> ScoutRegistrationProvider {
        ScoutRegistrationProvider::with_assets(
            &self.database,
            Arc::new(DraftStateManager::default()),
            &self.dotabase,
            &self.cache,
            timeout,
        )
        .expect("construct Scout provider")
    }
}

#[derive(Default)]
struct Captured {
    immediate: Vec<InteractionResponse>,
    defers: Vec<bool>,
    followups: Vec<InteractionResponse>,
    updates: Vec<InteractionResponse>,
    deletions: Vec<InteractionMessageReceipt>,
}

#[derive(Default)]
struct CapturingResponder {
    captured: Mutex<Captured>,
}

#[async_trait]
impl InteractionResponder for CapturingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("capture responses")
            .immediate
            .push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("capture responses")
            .defers
            .push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("capture responses")
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
            message_id: 55,
            channel_id: 77,
            delivery: InteractionMessageDelivery::InteractionFollowup,
        }))
    }

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("capture responses")
            .updates
            .push(response);
        Ok(())
    }

    async fn delete_message(
        &self,
        receipt: InteractionMessageReceipt,
    ) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("capture responses")
            .deletions
            .push(receipt);
        Ok(())
    }
}

fn registry(provider: &ScoutRegistrationProvider) -> Registry {
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(provider)
        .expect("register Scout provider");
    builder.build()
}

fn command(subcommand_name: &str, children: Vec<InteractionOption>) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "scout".to_owned(),
        user_id: USER as u64,
        user_display_name: "Alice".to_owned(),
        guild_id: Some(GUILD as u64),
        channel_id: Some(9),
        member_permissions: None,
        options: vec![InteractionOption {
            name: subcommand_name.to_owned(),
            value: InteractionValue::Subcommand(children),
        }],
    }
}

fn players(value: &str) -> InteractionOption {
    InteractionOption {
        name: "players".to_owned(),
        value: InteractionValue::String(value.to_owned()),
    }
}

#[test]
fn registration_matches_the_two_subcommand_python_surface() {
    let fixture = Fixture::new();
    let registry = registry(&fixture.provider(VIEW_TIMEOUT));
    let command = registry.commands().next().expect("Scout command");
    assert_eq!(
        (command.name.as_str(), command.description.as_str()),
        ("scout", "Player scouting tools")
    );
    assert_eq!(command.options.len(), 2);
    assert_eq!(command.options[0].name, "report");
    assert_eq!(command.options[1].name, "links");
    assert!(command.options.iter().all(|option| {
        option.kind == CommandOptionKind::Subcommand
            && option
                .options
                .iter()
                .map(|child| child.name.as_str())
                .eq(["players", "team", "lobby"])
    }));
    assert_eq!(
        registry.component_routes()[0].custom_id_prefix,
        COMPONENT_PREFIX
    );
}

#[tokio::test]
async fn migrated_sqlite_report_renders_native_png_pages_and_enforces_owner() {
    let fixture = Fixture::new();
    fixture.register(USER, "Alice", Some(1234));
    fixture.seed_hero_rows(12);
    let portrait = NativeScoutImageRenderer::default()
        .render(&ScoutReportInput {
            scout_data: ScoutData {
                player_count: 0,
                total_matches: Some(0),
                heroes: Vec::new(),
            },
            player_names: Vec::new(),
            title: "portrait fixture".to_owned(),
        })
        .expect("render cached portrait fixture");
    fixture.cache_portrait(1, &portrait);
    let registry = registry(&fixture.provider(VIEW_TIMEOUT));
    let handler = registry.command_handler("scout").expect("Scout handler");
    let responder = Arc::new(CapturingResponder::default());
    handler
        .handle(
            command("report", vec![players("<@100>")]),
            responder.clone(),
        )
        .await
        .expect("render Scout report");

    let next_id = {
        let captured = responder.captured.lock().expect("captured Scout report");
        assert_eq!(captured.defers, [false]);
        assert_eq!(captured.followups.len(), 1);
        let response = &captured.followups[0];
        assert_eq!(response.embeds[0].title.as_deref(), Some("SCOUT: 1 Player"));
        assert_eq!(
            response.embeds[0].description.as_deref(),
            Some("1 players | Heroes 1-10 of 12")
        );
        assert_eq!(
            response.embeds[0].image_url.as_deref(),
            Some("attachment://scout_report.png")
        );
        assert!(
            response.attachments[0]
                .bytes
                .starts_with(b"\x89PNG\r\n\x1a\n")
        );
        assert!(fixture.cache.join("heroes/1.png").exists());
        assert!(response.components[0].buttons[0].disabled);
        assert!(!response.components[0].buttons[1].disabled);
        response.components[0].buttons[1].custom_id.clone()
    };

    let component = registry
        .component_handler(&next_id)
        .expect("Scout component route");
    component
        .handle(
            InteractionRequest::Component {
                interaction_id: 2,
                custom_id: next_id.clone(),
                user_id: 999,
                user_display_name: "Intruder".to_owned(),
                guild_id: Some(GUILD as u64),
                channel_id: Some(9),
                member_permissions: None,
                values: Vec::new(),
            },
            responder.clone(),
        )
        .await
        .expect("reject non-owner paging");
    component
        .handle(
            InteractionRequest::Component {
                interaction_id: 3,
                custom_id: next_id,
                user_id: USER as u64,
                user_display_name: "Alice".to_owned(),
                guild_id: Some(GUILD as u64),
                channel_id: Some(9),
                member_permissions: None,
                values: Vec::new(),
            },
            responder.clone(),
        )
        .await
        .expect("advance Scout page");

    let captured = responder.captured.lock().expect("captured paging");
    assert!(
        captured
            .immediate
            .last()
            .expect("owner rejection")
            .ephemeral
    );
    assert_eq!(captured.updates.len(), 1);
    assert_eq!(
        captured.updates[0].embeds[0].description.as_deref(),
        Some("1 players | Heroes 11-12 of 12")
    );
    assert!(captured.updates[0].components[0].buttons[1].disabled);
}

#[test]
fn test_fetch_persists_original_and_reloads_without_http() {
    let directory = tempfile::tempdir().expect("portrait cache directory");
    let payload = portrait_png();
    let server = PortraitServer::start(1, payload.clone(), Duration::ZERO);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("portrait client");

    let first = fetch_portrait(
        7,
        &format!("{}/7.png", server.base_url),
        directory.path(),
        &client,
    )
    .expect("HTTP portrait");
    assert_eq!(first, payload);
    let path = directory.path().join("heroes/7.png");
    assert_eq!(std::fs::read(&path).expect("cached portrait"), payload);

    let second = fetch_portrait(
        7,
        "http://127.0.0.1:1/must-not-be-called.png",
        directory.path(),
        &client,
    )
    .expect("disk portrait");
    assert_eq!(second, first);
    assert_eq!(server.paths.lock().expect("portrait paths").len(), 1);
}

#[test]
fn test_corrupt_disk_entry_is_replaced_by_valid_http_image() {
    let directory = tempfile::tempdir().expect("portrait cache directory");
    let path = directory.path().join("heroes/9.png");
    std::fs::create_dir_all(path.parent().expect("portrait parent"))
        .expect("create portrait cache");
    std::fs::write(&path, b"not an image").expect("write corrupt portrait");
    let payload = portrait_png();
    let server = PortraitServer::start(1, payload.clone(), Duration::ZERO);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("portrait client");

    let fetched = fetch_portrait(
        9,
        &format!("{}/9.png", server.base_url),
        directory.path(),
        &client,
    )
    .expect("replacement portrait");

    assert_eq!(fetched, payload);
    assert_eq!(std::fs::read(path).expect("replacement cache"), payload);
}

#[test]
fn test_batch_deduplicates_and_bounds_parallel_missing_fetches() {
    let directory = tempfile::tempdir().expect("portrait cache directory");
    let payload = portrait_png();
    let cached_path = directory.path().join("heroes/99.png");
    std::fs::create_dir_all(cached_path.parent().expect("portrait parent"))
        .expect("create portrait cache");
    std::fs::write(&cached_path, &payload).expect("seed cached portrait");
    let server = PortraitServer::start(4, payload.clone(), Duration::from_millis(30));
    let hero_urls = [1, 2, 3, 4, 99]
        .into_iter()
        .map(|hero_id| (hero_id, format!("{}/{hero_id}.png", server.base_url)))
        .collect();
    let heroes = [1, 2, 1, 99, 3, 2, 4]
        .into_iter()
        .map(scout_hero)
        .collect::<Vec<_>>();

    let portraits = load_portraits(&heroes, &hero_urls, directory.path());

    let paths = server.paths.lock().expect("portrait paths").clone();
    assert_eq!(paths.len(), 4);
    for hero_id in [1, 2, 3, 4] {
        assert_eq!(
            paths
                .iter()
                .filter(|path| path.starts_with(&format!("GET /{hero_id}.png ")))
                .count(),
            1
        );
    }
    assert!((2..=4).contains(&server.max_active.load(Ordering::SeqCst)));
    assert_eq!(
        portraits.keys().copied().collect::<Vec<_>>(),
        [1, 2, 3, 4, 99]
    );
    assert!(portraits.values().all(|bytes| bytes == &payload));
}

#[tokio::test]
async fn links_disclose_only_guild_registered_players_and_preserve_mentions() {
    let fixture = Fixture::new();
    fixture.register(USER, "Alice", Some(1234));
    let registry = registry(&fixture.provider(VIEW_TIMEOUT));
    let responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("scout")
        .expect("Scout handler")
        .handle(
            command("links", vec![players("<@100> <@999>")]),
            responder.clone(),
        )
        .await
        .expect("render Scout links");

    let captured = responder.captured.lock().expect("captured Scout links");
    assert_eq!(captured.defers, [false]);
    let embed = &captured.followups[0].embeds[0];
    assert_eq!(embed.title.as_deref(), Some("Dotabuff Links — 2 Players"));
    assert!(
        embed.fields[0]
            .value
            .contains("**Alice** — [Dotabuff](https://www.dotabuff.com/players/1234)")
    );
    assert!(
        embed.fields[0]
            .value
            .contains("<@999> — not registered in this server")
    );
}

#[tokio::test]
async fn view_timeout_deletes_the_followup_and_stale_components_fail_safely() {
    let fixture = Fixture::new();
    fixture.register(USER, "Alice", None);
    fixture.seed_hero_rows(1);
    let registry = registry(&fixture.provider(Duration::from_millis(5)));
    let responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("scout")
        .expect("Scout handler")
        .handle(
            command("report", vec![players("<@100>")]),
            responder.clone(),
        )
        .await
        .expect("render Scout report");
    let custom_id = responder.captured.lock().expect("capture").followups[0].components[0].buttons
        [1]
    .custom_id
    .clone();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        responder.captured.lock().expect("capture").deletions.len(),
        1
    );

    registry
        .component_handler(&custom_id)
        .expect("Scout component")
        .handle(
            InteractionRequest::Component {
                interaction_id: 4,
                custom_id,
                user_id: USER as u64,
                user_display_name: "Alice".to_owned(),
                guild_id: Some(GUILD as u64),
                channel_id: Some(9),
                member_permissions: None,
                values: Vec::new(),
            },
            responder.clone(),
        )
        .await
        .expect("ignore stale Scout component");
}

#[tokio::test]
async fn guild_guard_is_ephemeral_and_precedes_defer() {
    let fixture = Fixture::new();
    let registry = registry(&fixture.provider(VIEW_TIMEOUT));
    let responder = Arc::new(CapturingResponder::default());
    let mut request = command("report", Vec::new());
    if let InteractionRequest::Command { guild_id, .. } = &mut request {
        *guild_id = None;
    }
    registry
        .command_handler("scout")
        .expect("Scout handler")
        .handle(request, responder.clone())
        .await
        .expect("apply guild guard");
    let captured = responder.captured.lock().expect("capture guild guard");
    assert!(captured.defers.is_empty());
    assert_eq!(captured.immediate[0].content, GUILD_ONLY_MESSAGE);
    assert!(captured.immediate[0].ephemeral);
}

#[test]
fn custom_dotabase_fixture_is_read_only_after_construction() {
    let fixture = Fixture::new();
    let before = std::fs::metadata(&fixture.dotabase)
        .expect("Dotabase metadata")
        .len();
    let _provider = fixture.provider(VIEW_TIMEOUT);
    let after = std::fs::metadata(&fixture.dotabase)
        .expect("Dotabase metadata")
        .len();
    assert_eq!(before, after);
    assert!(Path::new(&fixture.cache).starts_with(fixture._directory.path()));
}
