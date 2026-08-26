use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use cama_app::wrapped_media::{WrappedSlideData, render_wrapped_slide};
use rusqlite::{Connection, params};
use tempfile::TempDir;

use super::*;
use crate::registration::{
    InteractionMessageDelivery, InteractionResponseError, Registry, RegistryBuilder,
};
use crate::test_support::initialize_test_database as initialize_or_migrate;

const GUILD: u64 = 42;
const OWNER: u64 = 100;

struct Fixture {
    _directory: TempDir,
    path: std::path::PathBuf,
}

impl Fixture {
    fn migrated() -> Self {
        let directory = tempfile::tempdir().expect("temporary Wrapped directory");
        let path = directory.path().join("cama.sqlite3");
        initialize_or_migrate(&path).expect("migrate Wrapped database");
        let connection = Connection::open(&path).expect("open Wrapped fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("disable fixture foreign keys");
        for (id, name) in [
            (100_i64, "Database Owner"),
            (200, "Database Ally"),
            (300, "Database Rival"),
            (400, "Database Peer"),
        ] {
            connection
                .execute(
                    "INSERT INTO players(
                         discord_id,guild_id,discord_username,jopacoin_balance,
                         lowest_balance_ever,total_bets_placed
                     ) VALUES(?1,42,?2,1000,-50,3)",
                    params![id, name],
                )
                .expect("insert Wrapped player");
        }
        for match_id in 1_i64..=3 {
            let won = match_id != 2;
            let winning_team = if won { 1 } else { 2 };
            connection
                .execute(
                    "INSERT INTO matches(
                         match_id,team1_players,team2_players,winning_team,match_date,
                         guild_id,duration_seconds,radiant_score,dire_score,valve_match_id
                     ) VALUES(?1,'[100,200]','[300,400]',?2,?3,42,?4,35,24,?5)",
                    params![
                        match_id,
                        winning_team,
                        format!("2026-0{match_id}-01 12:00:00"),
                        1800 + match_id * 300,
                        9000 + match_id,
                    ],
                )
                .expect("insert Wrapped match");
            connection
                .execute(
                    "INSERT INTO match_participants(
                         match_id,discord_id,guild_id,team_number,side,won,hero_id,
                         kills,deaths,assists,last_hits,denies,gpm,xpm,hero_damage,
                         tower_damage,hero_healing,obs_placed,sen_placed,stuns,
                         towers_killed,fantasy_points
                     ) VALUES(?1,100,42,1,'radiant',?2,?3,?4,?5,?6,?7,?8,?9,
                              ?10,?11,?12,?13,?14,?15,?16,?17,?18)",
                    params![
                        match_id,
                        won,
                        match_id,
                        8 + match_id,
                        2 + match_id,
                        10 + match_id,
                        100 + match_id * 10,
                        match_id,
                        500 + match_id * 20,
                        600 + match_id * 20,
                        10_000 + match_id * 1000,
                        2_000 + match_id * 500,
                        100 * match_id,
                        match_id,
                        match_id + 1,
                        5.0 + match_id as f64,
                        match_id,
                        20.0 + match_id as f64,
                    ],
                )
                .expect("insert target participant");
            connection
                .execute(
                    "INSERT INTO match_participants(
                         match_id,discord_id,guild_id,team_number,side,won,hero_id,
                         kills,deaths,assists,gpm,xpm,fantasy_points
                     ) VALUES(?1,200,42,1,'radiant',?2,2,5,4,8,400,450,10)",
                    params![match_id, won],
                )
                .expect("insert comparison participant");
            connection
                .execute(
                    "INSERT INTO wrapped_enrichment_facts(
                         guild_id,match_id,discord_id,actions_per_min,courier_kills,
                         pings,rapier_count,lane_role,comeback,throw
                     ) VALUES(42,?1,100,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        match_id,
                        200 + match_id * 10,
                        match_id,
                        50 + match_id * 5,
                        i64::from(match_id == 3),
                        match_id,
                        1000 * match_id,
                        500 * match_id,
                    ],
                )
                .expect("insert compact Wrapped facts");
            connection
                .execute(
                    "INSERT INTO bets(
                         guild_id,match_id,discord_id,team_bet_on,amount,leverage,
                         payout,bet_time,is_blind
                     ) VALUES(42,?1,100,'radiant',100,1,?2,?3,0)",
                    params![
                        match_id,
                        if won { Some(220_i64) } else { None },
                        1_770_000_000 + match_id,
                    ],
                )
                .expect("insert Wrapped bet");
            connection
                .execute(
                    "INSERT INTO rating_history(
                         discord_id,guild_id,rating_before,rating,timestamp,match_id
                     ) VALUES(100,42,?1,?2,?3,?4)",
                    params![
                        1450.0 + match_id as f64 * 20.0,
                        1470.0 + match_id as f64 * 20.0,
                        format!("2026-0{match_id}-01 13:00:00"),
                        match_id,
                    ],
                )
                .expect("insert target rating history");
        }
        connection
            .execute(
                "INSERT INTO rating_history(
                     discord_id,guild_id,rating_before,rating,timestamp,match_id
                 ) VALUES(200,42,1600,1500,'2026-02-02 13:00:00',2)",
                [],
            )
            .expect("insert comparison rating history");
        for (peer, together, together_wins, against, owner_wins) in [
            (200_i64, 5_i64, 4_i64, 5_i64, 1_i64),
            (300, 5, 3, 5, 4),
            (400, 3, 2, 3, 2),
        ] {
            connection
                .execute(
                    "INSERT INTO player_pairings(
                         guild_id,player1_id,player2_id,games_together,wins_together,
                         games_against,player1_wins_against,last_match_id
                     ) VALUES(42,100,?1,?2,?3,?4,?5,3)",
                    params![peer, together, together_wins, against, owner_wins],
                )
                .expect("insert Wrapped pairings");
        }
        connection
            .execute(
                "INSERT INTO package_deal_purchases(
                     guild_id,buyer_discord_id,partner_discord_id,jc_spent,
                     games_committed,created_at
                 ) VALUES(42,100,200,750,5,1780272000)",
                [],
            )
            .expect("insert immutable package purchase");
        connection
            .execute(
                "INSERT INTO bankruptcy_state(
                     discord_id,guild_id,last_bankruptcy_at,bankruptcy_count
                 ) VALUES(100,42,1780272000,2)",
                [],
            )
            .expect("insert Wrapped bankruptcy");
        Self {
            _directory: directory,
            path,
        }
    }
}

struct FakeDiscord {
    profiles: BTreeMap<u64, WrappedDiscordProfile>,
    active: AtomicUsize,
    peak: AtomicUsize,
}

impl FakeDiscord {
    fn production() -> Arc<Self> {
        let mut avatar_slide = WrappedSlideData::new("story_games", "Avatar");
        avatar_slide.stat_value = "A".to_owned();
        let avatar = render_wrapped_slide(&avatar_slide).expect("native avatar fixture");
        Arc::new(Self {
            profiles: BTreeMap::from([
                (
                    100,
                    WrappedDiscordProfile {
                        display_name: "Guild Owner".to_owned(),
                        avatar_png: None,
                    },
                ),
                (
                    200,
                    WrappedDiscordProfile {
                        display_name: "Guild Ally".to_owned(),
                        avatar_png: Some(avatar),
                    },
                ),
                (
                    300,
                    WrappedDiscordProfile {
                        display_name: "Guild Rival".to_owned(),
                        avatar_png: None,
                    },
                ),
                (
                    400,
                    WrappedDiscordProfile {
                        display_name: "Guild Peer".to_owned(),
                        avatar_png: None,
                    },
                ),
            ]),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl WrappedDiscordPort for FakeDiscord {
    async fn wrapped_profile(
        &self,
        _guild_id: u64,
        user_id: u64,
        include_avatar: bool,
    ) -> Result<Option<WrappedDiscordProfile>, String> {
        let active = self.active.fetch_add(1, AtomicOrdering::SeqCst) + 1;
        self.peak.fetch_max(active, AtomicOrdering::SeqCst);
        tokio::time::sleep(Duration::from_millis(2)).await;
        self.active.fetch_sub(1, AtomicOrdering::SeqCst);
        Ok(self.profiles.get(&user_id).cloned().map(|mut profile| {
            if !include_avatar {
                profile.avatar_png = None;
            }
            profile
        }))
    }
}

struct ProfileProbe {
    profiles: BTreeMap<u64, WrappedDiscordProfile>,
    failures: BTreeSet<u64>,
    calls: Mutex<Vec<(u64, bool)>>,
    active: AtomicUsize,
    peak: AtomicUsize,
}

impl ProfileProbe {
    fn new(profiles: BTreeMap<u64, WrappedDiscordProfile>) -> Arc<Self> {
        Arc::new(Self {
            profiles,
            failures: BTreeSet::new(),
            calls: Mutex::new(Vec::new()),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }

    fn with_failures(
        profiles: BTreeMap<u64, WrappedDiscordProfile>,
        failures: BTreeSet<u64>,
    ) -> Arc<Self> {
        Arc::new(Self {
            profiles,
            failures,
            calls: Mutex::new(Vec::new()),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl WrappedDiscordPort for ProfileProbe {
    async fn wrapped_profile(
        &self,
        _guild_id: u64,
        user_id: u64,
        include_avatar: bool,
    ) -> Result<Option<WrappedDiscordProfile>, String> {
        self.calls
            .lock()
            .expect("profile probe calls")
            .push((user_id, include_avatar));
        let active = self.active.fetch_add(1, AtomicOrdering::SeqCst) + 1;
        self.peak.fetch_max(active, AtomicOrdering::SeqCst);
        tokio::time::sleep(Duration::from_millis(5)).await;
        self.active.fetch_sub(1, AtomicOrdering::SeqCst);
        if self.failures.contains(&user_id) {
            return Err("member unavailable".to_owned());
        }
        Ok(self.profiles.get(&user_id).cloned().map(|mut profile| {
            if !include_avatar {
                profile.avatar_png = None;
            }
            profile
        }))
    }
}

#[tokio::test]
async fn test_prefetch_profiles_reuses_members_for_names_and_avatars() {
    let fixture = Fixture::migrated();
    let probe = ProfileProbe::new(BTreeMap::from([
        (
            1,
            WrappedDiscordProfile {
                display_name: "One".to_owned(),
                avatar_png: Some(vec![1]),
            },
        ),
        (
            2,
            WrappedDiscordProfile {
                display_name: "Two".to_owned(),
                avatar_png: Some(vec![2]),
            },
        ),
        (
            3,
            WrappedDiscordProfile {
                display_name: "Three".to_owned(),
                avatar_png: None,
            },
        ),
    ]));
    let provider = build_provider(
        &fixture,
        Arc::clone(&probe) as Arc<dyn WrappedDiscordPort>,
        Duration::from_secs(1),
    );

    let profiles = provider
        .handler
        .prefetch_profiles(GUILD, BTreeSet::from([1, 2, 3]), BTreeSet::from([1, 2]))
        .await;

    assert_eq!(
        profiles
            .iter()
            .map(|(id, profile)| (*id, profile.display_name.as_str()))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([(1, "One"), (2, "Two"), (3, "Three")])
    );
    assert_eq!(profiles[&1].avatar_png, Some(vec![1]));
    assert_eq!(profiles[&2].avatar_png, Some(vec![2]));
    assert_eq!(profiles[&3].avatar_png, None);
    let mut calls = probe.calls.lock().expect("profile probe calls").clone();
    calls.sort_unstable();
    assert_eq!(calls, [(1, true), (2, true), (3, false)]);
}

#[tokio::test]
async fn test_prefetch_profiles_bounds_all_network_concurrency() {
    let fixture = Fixture::migrated();
    let probe = ProfileProbe::new(
        (0..12)
            .map(|id| {
                (
                    id,
                    WrappedDiscordProfile {
                        display_name: id.to_string(),
                        avatar_png: Some(vec![u8::try_from(id).expect("small fixture id")]),
                    },
                )
            })
            .collect(),
    );
    let provider = build_provider(
        &fixture,
        Arc::clone(&probe) as Arc<dyn WrappedDiscordPort>,
        Duration::from_secs(1),
    );
    let ids = (0..12).collect::<BTreeSet<_>>();

    let profiles = provider
        .handler
        .prefetch_profiles(GUILD, ids.clone(), ids)
        .await;

    assert_eq!(profiles.len(), 12);
    assert_eq!(probe.peak.load(AtomicOrdering::SeqCst), PROFILE_CONCURRENCY);
}

#[tokio::test]
async fn test_prefetch_profiles_isolates_member_and_avatar_failures() {
    let fixture = Fixture::migrated();
    let probe = ProfileProbe::with_failures(
        BTreeMap::from([
            (
                1,
                WrappedDiscordProfile {
                    display_name: "One".to_owned(),
                    // The live port preserves the member/display name when
                    // its bounded avatar download fails.
                    avatar_png: None,
                },
            ),
            (
                2,
                WrappedDiscordProfile {
                    display_name: "Two".to_owned(),
                    avatar_png: Some(vec![2]),
                },
            ),
        ]),
        BTreeSet::from([2]),
    );
    let provider = build_provider(
        &fixture,
        Arc::clone(&probe) as Arc<dyn WrappedDiscordPort>,
        Duration::from_secs(1),
    );

    let profiles = provider
        .handler
        .prefetch_profiles(GUILD, BTreeSet::from([1, 2]), BTreeSet::from([1, 2]))
        .await;

    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[&1].display_name, "One");
    assert_eq!(profiles[&1].avatar_png, None);
    assert!(!profiles.contains_key(&2));
}

#[derive(Default)]
struct Responder {
    immediate: Mutex<Vec<InteractionResponse>>,
    defers: Mutex<Vec<bool>>,
    followups: Mutex<Vec<InteractionResponse>>,
    updates: Mutex<Vec<InteractionResponse>>,
    deleted: Mutex<Vec<InteractionMessageReceipt>>,
}

#[async_trait]
impl InteractionResponder for Responder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.immediate.lock().expect("immediate").push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.defers.lock().expect("defers").push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.followups.lock().expect("followups").push(response);
        Ok(())
    }

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        self.followups.lock().expect("followups").push(response);
        Ok(Some(InteractionMessageReceipt {
            message_id: 77,
            channel_id: 88,
            delivery: InteractionMessageDelivery::InteractionFollowup,
        }))
    }

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.updates.lock().expect("updates").push(response);
        Ok(())
    }

    async fn delete_message(
        &self,
        receipt: InteractionMessageReceipt,
    ) -> Result<(), InteractionResponseError> {
        self.deleted.lock().expect("deleted").push(receipt);
        Ok(())
    }
}

fn config() -> ApplicationConfig {
    ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "WRAPPED_MIN_GAMES" | "WRAPPED_MIN_BETS" => Some("3".to_owned()),
        _ => None,
    })
    .expect("Wrapped test config")
}

fn build_provider(
    fixture: &Fixture,
    discord: Arc<dyn WrappedDiscordPort>,
    timeout: Duration,
) -> WrappedRegistrationProvider {
    WrappedRegistrationProvider::with_timeout_and_year(
        &fixture.path,
        &config(),
        discord,
        timeout,
        Some(2026),
    )
}

fn build_registry(provider: &WrappedRegistrationProvider) -> Registry {
    let mut builder = RegistryBuilder::default();
    builder.add_provider(provider).expect("register Wrapped");
    builder.build()
}

fn command(user_id: u64, guild_id: Option<u64>) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "wrapped".to_owned(),
        user_id,
        user_display_name: "Command Owner".to_owned(),
        guild_id,
        channel_id: Some(7),
        member_permissions: None,
        options: Vec::new(),
    }
}

fn component(custom_id: &str, user_id: u64) -> InteractionRequest {
    InteractionRequest::Component {
        interaction_id: 2,
        custom_id: custom_id.to_owned(),
        user_id,
        user_display_name: format!("User {user_id}"),
        guild_id: Some(GUILD),
        channel_id: Some(7),
        member_permissions: None,
        values: Vec::new(),
    }
}

fn assert_native_png(response: &InteractionResponse) {
    let bytes = &response.attachments[0].bytes;
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 800);
    assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 600);
    assert!(bytes.len() > 1_000);
}

#[test]
fn exact_python_slash_schema_and_component_route_are_registered() {
    let fixture = Fixture::migrated();
    let provider = build_provider(&fixture, FakeDiscord::production(), STORY_TIMEOUT);
    let registry = build_registry(&provider);
    let command = registry.commands().next().expect("Wrapped command");
    assert_eq!(command.name, "wrapped");
    assert_eq!(command.description, "View your Cama Wrapped year in review");
    assert_eq!(command.options.len(), 1);
    assert_eq!(command.options[0].name, "user");
    assert_eq!(
        command.options[0].description,
        "View another user's wrapped"
    );
    assert_eq!(command.options[0].kind, CommandOptionKind::User);
    assert!(!command.options[0].required);
    assert_eq!(registry.component_routes()[0].custom_id_prefix, "wrapped:");
}

#[tokio::test]
async fn migrated_sqlite_builds_all_sixteen_native_pages_and_live_navigation() {
    let fixture = Fixture::migrated();
    let discord = FakeDiscord::production();
    let provider = build_provider(&fixture, discord.clone(), STORY_TIMEOUT);
    let registry = build_registry(&provider);
    let handler = registry
        .command_handler("wrapped")
        .expect("Wrapped handler");
    let responder = Arc::new(Responder::default());
    handler
        .handle(command(OWNER, Some(GUILD)), responder.clone())
        .await
        .expect("run Wrapped command");

    assert_eq!(*responder.defers.lock().unwrap(), vec![false]);
    let next_id = {
        let followups = responder.followups.lock().unwrap();
        assert_eq!(followups.len(), 1);
        assert!(!followups[0].ephemeral);
        assert_eq!(
            followups[0].content,
            "**Cama Wrapped 2026** for Guild Owner"
        );
        assert_native_png(&followups[0]);
        assert_eq!(followups[0].components[0].buttons.len(), 2);
        assert!(followups[0].components[0].buttons[0].disabled);
        assert!(!followups[0].components[0].buttons[1].disabled);
        followups[0].components[0].buttons[1].custom_id.clone()
    };

    let session = provider
        .handler
        .sessions
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .expect("active Wrapped session");
    let locked = session.lock().await;
    assert!(
        locked
            .slides
            .iter()
            .all(|slide| slide.username == "Guild Owner")
    );
    assert!(
        locked.slides[0]
            .lines
            .iter()
            .any(|line| line.starts_with("TOP PERFORMER - Guild "))
    );
    assert_eq!(
        locked
            .slides
            .iter()
            .map(|slide| slide.kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "server_summary",
            "awards",
            "story_games",
            "story_summary",
            "records_combat",
            "records_farming",
            "records_impact",
            "records_vision",
            "records_endurance",
            "story_hero",
            "story_lanes",
            "story_teammates",
            "story_rivals",
            "story_packages",
            "chart_rating",
            "chart_gamba",
        ]
    );
    for slide in &locked.slides {
        let bytes = render_wrapped_slide(slide).expect("render every native story page");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }
    assert!(
        locked.slides[11]
            .lines
            .iter()
            .any(|line| line.contains("Guild Ally"))
    );
    assert!(
        locked.slides[11]
            .lines
            .iter()
            .all(|line| !line.contains("Database Ally"))
    );
    assert!(!locked.slides[11].avatars.is_empty());
    let wrapped_gamba = locked
        .slides
        .iter()
        .find(|slide| slide.kind == "chart_gamba")
        .expect("live Wrapped Gamba slide");
    assert_eq!(wrapped_gamba.title, "Gamba (All-Time)");
    assert_eq!(wrapped_gamba.lines, ["+140 JC · 3 bets · Degen Score: 47"]);
    let gamba = wrapped_gamba.gamba.as_ref().expect("typed Gamba payload");
    assert!(!gamba.points.is_empty());
    assert_eq!(gamba.points[0].event_number, 1);
    assert_eq!(gamba.stats.total_bets, 3);
    assert_eq!(gamba.degen_score, 47);
    assert_eq!(gamba.degen_title, "Committed");
    drop(locked);
    assert!(discord.peak.load(AtomicOrdering::SeqCst) <= PROFILE_CONCURRENCY);

    handler
        .handle(component(&next_id, OWNER), responder.clone())
        .await
        .expect("navigate next");
    {
        let updates = responder.updates.lock().unwrap();
        assert_eq!(updates.len(), 1);
        assert_native_png(&updates[0]);
        assert!(!updates[0].components[0].buttons[0].disabled);
    }
    assert_eq!(session.lock().await.current, 1);
    assert_eq!(session.lock().await.rendered.len(), 2);
    assert_eq!(session.lock().await.expiry_generation, 1);

    handler
        .handle(component(&next_id, 999), responder.clone())
        .await
        .expect("reject another navigator");
    assert_eq!(
        responder.immediate.lock().unwrap().last().unwrap().content,
        "Only the command invoker can navigate this wrapped."
    );

    let restarted = build_provider(&fixture, FakeDiscord::production(), STORY_TIMEOUT);
    let restarted_registry = build_registry(&restarted);
    let restarted_responder = Arc::new(Responder::default());
    restarted_registry
        .command_handler("wrapped")
        .unwrap()
        .handle(command(OWNER, Some(GUILD)), restarted_responder.clone())
        .await
        .expect("create post-restart story");
    let restarted_next_id = restarted_responder.followups.lock().unwrap()[0].components[0].buttons
        [1]
    .custom_id
    .clone();
    assert_ne!(next_id, restarted_next_id);
    restarted_registry
        .component_handler(&next_id)
        .expect("restart retains Wrapped component route")
        .handle(component(&next_id, OWNER), responder.clone())
        .await
        .expect("reject stale restart component");
    assert_eq!(
        responder.immediate.lock().unwrap().last().unwrap().content,
        "This wrapped has expired."
    );
}

#[tokio::test]
async fn guild_guard_cooldown_and_no_data_paths_preserve_privacy_contracts() {
    let fixture = Fixture::migrated();
    let provider = build_provider(&fixture, FakeDiscord::production(), STORY_TIMEOUT);
    let registry = build_registry(&provider);
    let handler = registry.command_handler("wrapped").unwrap();
    let responder = Arc::new(Responder::default());
    handler
        .handle(command(500, None), responder.clone())
        .await
        .unwrap();
    let response = responder.immediate.lock().unwrap()[0].clone();
    assert!(response.ephemeral);
    assert_eq!(
        response.content,
        "This command can only be used in a server."
    );
    handler
        .handle(command(500, Some(GUILD)), responder.clone())
        .await
        .unwrap();
    let dm_cooldown = responder.immediate.lock().unwrap().last().unwrap().clone();
    assert!(dm_cooldown.ephemeral);
    assert_eq!(
        dm_cooldown.content,
        "❌ An error occurred while processing your command. Please try again."
    );

    handler
        .handle(command(OWNER, Some(GUILD)), responder.clone())
        .await
        .unwrap();
    handler
        .handle(command(OWNER, Some(GUILD)), responder.clone())
        .await
        .unwrap();
    let cooldown = responder.immediate.lock().unwrap().last().unwrap().clone();
    assert!(cooldown.ephemeral);
    assert_eq!(
        cooldown.content,
        "❌ An error occurred while processing your command. Please try again."
    );

    let empty_directory = tempfile::tempdir().unwrap();
    let empty_path = empty_directory.path().join("empty.sqlite3");
    initialize_or_migrate(&empty_path).unwrap();
    let empty_provider = WrappedRegistrationProvider::with_timeout_and_year(
        &empty_path,
        &config(),
        FakeDiscord::production(),
        STORY_TIMEOUT,
        Some(2026),
    );
    let empty_registry = build_registry(&empty_provider);
    let empty_responder = Arc::new(Responder::default());
    empty_registry
        .command_handler("wrapped")
        .unwrap()
        .handle(command(700, Some(GUILD)), empty_responder.clone())
        .await
        .unwrap();
    assert_eq!(*empty_responder.defers.lock().unwrap(), vec![false]);
    let no_data = empty_responder.followups.lock().unwrap()[0].clone();
    assert!(no_data.ephemeral);
    assert_eq!(no_data.content, "No match data found for 2026.");
}

#[tokio::test]
async fn timeout_deletes_public_followup_and_late_button_is_private() {
    let fixture = Fixture::migrated();
    let provider = build_provider(
        &fixture,
        FakeDiscord::production(),
        Duration::from_millis(20),
    );
    let registry = build_registry(&provider);
    let handler = registry.command_handler("wrapped").unwrap();
    let responder = Arc::new(Responder::default());
    handler
        .handle(command(OWNER, Some(GUILD)), responder.clone())
        .await
        .unwrap();
    let next_id = responder.followups.lock().unwrap()[0].components[0].buttons[1]
        .custom_id
        .clone();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(provider.handler.sessions.lock().unwrap().is_empty());
    assert_eq!(
        *responder.deleted.lock().unwrap(),
        vec![InteractionMessageReceipt {
            message_id: 77,
            channel_id: 88,
            delivery: InteractionMessageDelivery::InteractionFollowup,
        }]
    );
    handler
        .handle(component(&next_id, OWNER), responder.clone())
        .await
        .unwrap();
    let expired = responder.immediate.lock().unwrap().last().unwrap().clone();
    assert!(expired.ephemeral);
    assert_eq!(expired.content, "This wrapped has expired.");
}

#[test]
fn awards_match_python_tie_exclusions_hero_thresholds_and_flavor() {
    let raw = WrappedRawData {
        summary: WrappedServerSummary::default(),
        player_stats: vec![
            WrappedPlayerStats {
                discord_id: 1,
                discord_username: "One".to_owned(),
                games_played: 4,
                wins: 3,
                losses: 0,
                avg_gpm: 500.0,
                avg_xpm: 600.0,
                avg_kda: 5.0,
                total_kills: 30,
                total_deaths: 6,
                total_assists: 30,
                total_wards: 10,
                total_fantasy: 30.0,
            },
            WrappedPlayerStats {
                discord_id: 2,
                discord_username: "Two".to_owned(),
                games_played: 3,
                wins: 0,
                losses: 3,
                avg_gpm: 300.0,
                avg_xpm: 400.0,
                avg_kda: 1.0,
                total_kills: 5,
                total_deaths: 20,
                total_assists: 15,
                total_wards: 5,
                total_fantasy: 10.0,
            },
        ],
        hero_stats: Vec::new(),
        player_heroes: vec![
            WrappedPlayerHeroStats {
                discord_id: 1,
                hero_id: 2,
                picks: 2,
                wins: 2,
                total_kills: 1,
                total_deaths: 1,
                total_assists: 1,
            },
            WrappedPlayerHeroStats {
                discord_id: 1,
                hero_id: 3,
                picks: 1,
                wins: 1,
                total_kills: 1,
                total_deaths: 1,
                total_assists: 1,
            },
            WrappedPlayerHeroStats {
                discord_id: 2,
                hero_id: 4,
                picks: 3,
                wins: 0,
                total_kills: 1,
                total_deaths: 1,
                total_assists: 1,
            },
        ],
        rating_changes: vec![WrappedRatingChange {
            discord_id: 1,
            discord_username: "One".to_owned(),
            first_rating: 1000.0,
            last_rating: 1000.0,
            rating_change: 0.0,
            rating_variance: Some(0.0),
        }],
        betting: vec![WrappedBettingStats {
            discord_id: 1,
            discord_username: "One".to_owned(),
            total_bets: 3,
            total_wagered: 100,
            net_pnl: 0,
            wins: 1,
            losses: 2,
        }],
        bets_against: Vec::new(),
        bankruptcies: Vec::new(),
        player_name: None,
        matches: Vec::new(),
        pairwise: PairwiseInput::default(),
        pairwise_names: BTreeMap::new(),
        package_purchases: Vec::new(),
        rating_history: Vec::new(),
        gamba_stats: None,
        gamba_points: Vec::new(),
    };
    let awards = generate_awards(&raw, 3, 3);
    let titles = awards.iter().map(|award| award.title).collect::<Vec<_>>();
    assert!(titles.contains(&"One-Trick Pony"));
    assert!(titles.contains(&"Jack of All Trades"));
    assert!(titles.contains(&"No Life"));
    assert!(titles.contains(&"Touched Grass"));
    assert_eq!(
        titles
            .iter()
            .filter(|title| **title == "Steady Eddie")
            .count(),
        1
    );
    assert!(!titles.contains(&"Coin Flip Player"));
    assert!(!titles.contains(&"Diamond Hands"));
    assert!(!titles.contains(&"House's Favorite"));
    assert!(awards.iter().all(|award| !award.flavor.is_empty()));

    let slides = build_slides(raw, &BTreeMap::new(), 1, "1", 2026, 3, 3);
    let rendered_lines = slides
        .iter()
        .flat_map(|slide| slide.lines.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered_lines.contains("TOP PERFORMER - 1 -"));
    assert!(!rendered_lines.contains("TOP PERFORMER - One -"));
}
