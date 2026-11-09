use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use cama_app::wrapped_media::{
    WrappedPunch, WrappedSlideData, backgrounds_alike, render_wrapped_slide, slide_backgrounds,
};
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

fn profile(name: &str) -> WrappedDiscordProfile {
    WrappedDiscordProfile {
        display_name: name.to_owned(),
    }
}

impl FakeDiscord {
    fn production() -> Arc<Self> {
        Arc::new(Self {
            profiles: BTreeMap::from([
                (100, profile("Guild Owner")),
                (200, profile("Guild Ally")),
                (300, profile("Guild Rival")),
                (400, profile("Guild Peer")),
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
    ) -> Result<Option<WrappedDiscordProfile>, String> {
        let active = self.active.fetch_add(1, AtomicOrdering::SeqCst) + 1;
        self.peak.fetch_max(active, AtomicOrdering::SeqCst);
        tokio::time::sleep(Duration::from_millis(2)).await;
        self.active.fetch_sub(1, AtomicOrdering::SeqCst);
        Ok(self.profiles.get(&user_id).cloned())
    }
}

struct ProfileProbe {
    profiles: BTreeMap<u64, WrappedDiscordProfile>,
    failures: BTreeSet<u64>,
    calls: Mutex<Vec<u64>>,
    active: AtomicUsize,
    peak: AtomicUsize,
}

impl ProfileProbe {
    fn new(profiles: BTreeMap<u64, WrappedDiscordProfile>) -> Arc<Self> {
        Self::with_failures(profiles, BTreeSet::new())
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
    ) -> Result<Option<WrappedDiscordProfile>, String> {
        self.calls
            .lock()
            .expect("profile probe calls")
            .push(user_id);
        let active = self.active.fetch_add(1, AtomicOrdering::SeqCst) + 1;
        self.peak.fetch_max(active, AtomicOrdering::SeqCst);
        tokio::time::sleep(Duration::from_millis(5)).await;
        self.active.fetch_sub(1, AtomicOrdering::SeqCst);
        if self.failures.contains(&user_id) {
            return Err("member unavailable".to_owned());
        }
        Ok(self.profiles.get(&user_id).cloned())
    }
}

#[tokio::test]
async fn test_prefetch_profiles_asks_once_per_member() {
    let fixture = Fixture::migrated();
    let probe = ProfileProbe::new(BTreeMap::from([
        (1, profile("One")),
        (2, profile("Two")),
        (3, profile("Three")),
    ]));
    let provider = build_provider(
        &fixture,
        Arc::clone(&probe) as Arc<dyn WrappedDiscordPort>,
        Duration::from_secs(1),
    );

    let profiles = provider
        .handler
        .prefetch_profiles(GUILD, BTreeSet::from([1, 2, 3]))
        .await;

    assert_eq!(
        profiles
            .iter()
            .map(|(id, profile)| (*id, profile.display_name.as_str()))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([(1, "One"), (2, "Two"), (3, "Three")])
    );
    let mut calls = probe.calls.lock().expect("profile probe calls").clone();
    calls.sort_unstable();
    assert_eq!(calls, [1, 2, 3]);
}

#[tokio::test]
async fn test_prefetch_profiles_bounds_all_network_concurrency() {
    let fixture = Fixture::migrated();
    let probe = ProfileProbe::new((0..12).map(|id| (id, profile(&id.to_string()))).collect());
    let provider = build_provider(
        &fixture,
        Arc::clone(&probe) as Arc<dyn WrappedDiscordPort>,
        Duration::from_secs(1),
    );

    let profiles = provider
        .handler
        .prefetch_profiles(GUILD, (0..12).collect())
        .await;

    assert_eq!(profiles.len(), 12);
    assert_eq!(probe.peak.load(AtomicOrdering::SeqCst), PROFILE_CONCURRENCY);
}

#[tokio::test]
async fn test_prefetch_profiles_isolates_member_failures() {
    let fixture = Fixture::migrated();
    let probe = ProfileProbe::with_failures(
        BTreeMap::from([(1, profile("One")), (2, profile("Two"))]),
        BTreeSet::from([2]),
    );
    let provider = build_provider(
        &fixture,
        Arc::clone(&probe) as Arc<dyn WrappedDiscordPort>,
        Duration::from_secs(1),
    );

    let profiles = provider
        .handler
        .prefetch_profiles(GUILD, BTreeSet::from([1, 2]))
        .await;

    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[&1].display_name, "One");
    assert!(!profiles.contains_key(&2));
}

#[derive(Default)]
struct Responder {
    immediate: Mutex<Vec<InteractionResponse>>,
    defers: Mutex<Vec<bool>>,
    followups: Mutex<Vec<InteractionResponse>>,
    updates: Mutex<Vec<InteractionResponse>>,
    deleted: Mutex<Vec<InteractionMessageReceipt>>,
    update_gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
    fail_update: std::sync::atomic::AtomicBool,
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
        let gate = self.update_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.notified().await;
        }
        if self.fail_update.load(AtomicOrdering::SeqCst) {
            return Err(InteractionResponseError::new("test delivery failure"));
        }
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
    assert_eq!(
        u32::from_be_bytes(bytes[16..20].try_into().unwrap()) as usize,
        cama_app::wrapped_media::WRAPPED_SLIDE_WIDTH
    );
    assert_eq!(
        u32::from_be_bytes(bytes[20..24].try_into().unwrap()) as usize,
        cama_app::wrapped_media::WRAPPED_SLIDE_HEIGHT
    );
    assert!(bytes.len() > 1_000);
}

fn assert_native_media(response: &InteractionResponse) {
    if response.attachments[0].filename.ends_with(".gif") {
        assert!(response.attachments[0].bytes.starts_with(b"GIF89a"));
    } else {
        assert_native_png(response);
    }
}

#[tokio::test]
async fn personal_story_offers_still_mode_recap_and_separate_details() {
    let fixture = Fixture::migrated();
    let provider = build_provider(&fixture, FakeDiscord::production(), STORY_TIMEOUT);
    let registry = build_registry(&provider);
    let handler = registry.command_handler("wrapped").unwrap();
    let responder = Arc::new(Responder::default());
    handler
        .handle(command(OWNER, Some(GUILD)), responder.clone())
        .await
        .unwrap();
    let (motion_id, recap_id, details_id) = {
        let messages = responder.followups.lock().unwrap();
        let message = &messages[0];
        let buttons = &message.components[0].buttons;
        let find = |label| {
            buttons
                .iter()
                .find(|button| button.label == label)
                .unwrap_or_else(|| panic!("missing {label} control"))
                .custom_id
                .clone()
        };
        assert!(message.attachments[0].bytes.starts_with(b"GIF89a"));
        (find("Still images"), find("My recap"), find("More stats"))
    };
    handler
        .handle(component(&motion_id, OWNER), responder.clone())
        .await
        .unwrap();
    assert_native_png(responder.updates.lock().unwrap().last().unwrap());
    handler
        .handle(component(&recap_id, OWNER), responder.clone())
        .await
        .unwrap();
    {
        let updates = responder.updates.lock().unwrap();
        let recap = updates.last().unwrap();
        assert_native_png(recap);
        assert!(
            recap.components[0]
                .buttons
                .iter()
                .any(|button| button.label == "Replay")
        );
    }
    handler
        .handle(component(&details_id, OWNER), responder.clone())
        .await
        .unwrap();
    let back_id;
    {
        let updates = responder.updates.lock().unwrap();
        let details = updates.last().unwrap();
        assert!(details.content.contains("More stats"));
        assert!(
            details.components[0]
                .buttons
                .iter()
                .any(|button| button.label == "Back to story")
        );
        back_id = details.components[0]
            .buttons
            .iter()
            .find(|button| button.label == "Back to story")
            .unwrap()
            .custom_id
            .clone();
    }
    handler
        .handle(component(&back_id, OWNER), responder.clone())
        .await
        .unwrap();
    let replay_id = responder.updates.lock().unwrap().last().unwrap().components[0].buttons[1]
        .custom_id
        .clone();
    assert!(replay_id.ends_with(":replay"));
    handler
        .handle(component(&replay_id, OWNER), responder.clone())
        .await
        .unwrap();
    assert!(
        responder
            .updates
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .content
            .contains("Chapter 1/")
    );
    let before = responder.updates.lock().unwrap().len();
    handler
        .handle(component(&motion_id, 999), responder.clone())
        .await
        .unwrap();
    assert_eq!(responder.updates.lock().unwrap().len(), before);
    assert!(
        responder
            .followups
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .ephemeral
    );
    let session = provider
        .handler
        .sessions
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();
    let session = session.lock().await;
    assert_eq!(session.slides.first().unwrap().kind, "story_intro");
    assert_eq!(session.slides.last().unwrap().kind, "story_finale");
    assert!(
        !session
            .slides
            .iter()
            .any(|slide| slide.kind.starts_with("records_"))
    );
    assert!(
        session
            .slides
            .iter()
            .any(|slide| slide.kind == "story_distinct")
    );
    assert!(
        session
            .slides
            .iter()
            .all(|slide| slide.kind != "story_moment")
    );
    assert!(
        session
            .slides
            .last()
            .unwrap()
            .lines
            .iter()
            .any(|line| line == "3|games played")
    );
    assert!(
        session
            .details
            .iter()
            .filter(|slide| slide.kind.starts_with("records_") || slide.kind == "awards")
            .all(|slide| slide.lines.len() <= 2)
    );
    for (index, slide) in session.slides.iter().enumerate() {
        assert_eq!(slide.chapter_index, index);
        assert_eq!(slide.chapter_count, session.slides.len());
    }
    // The rating chart is a detail page and only covers the requested year.
    let rating_chart = session
        .details
        .iter()
        .find(|slide| slide.kind == "chart_rating")
        .expect("fixture produces a rating chart detail page");
    assert_eq!(rating_chart.year_label, "Cama Wrapped 2026");
    assert!(
        session
            .slides
            .iter()
            .filter(|slide| slide.kind.starts_with("chart_gamba") || slide.kind == "story_gamba")
            .all(|slide| slide.year_label == "Cama Wrapped 2026")
    );
    assert!(!session.animated);
    assert!(
        session
            .rendered
            .values()
            .map(|media| media.bytes.len())
            .sum::<usize>()
            <= MAX_CACHED_MEDIA_BYTES
    );
}

#[test]
fn missing_or_unreadable_profiles_never_show_discord_ids() {
    let fixture = Fixture::migrated();
    let raw = WrappedSources::new(&fixture.path)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    let slides = build_slides(raw, &BTreeMap::new(), OWNER as i64, "<@100>", 2026, 3, 3);
    assert!(
        slides
            .iter()
            .all(|slide| slide.username == "Unknown player")
    );
    for slide in &slides {
        for text in slide_texts(slide) {
            assert!(!text.contains("<@"), "{}: {text}", slide.kind);
        }
    }
}

#[test]
fn sparse_wrapped_does_not_invent_zero_combat_or_identity() {
    let fixture = Fixture::migrated();
    let mut raw = WrappedSources::new(&fixture.path)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    raw.player_heroes.clear();
    raw.rating_changes.clear();
    for row in &mut raw.matches {
        row.hero_id = None;
        row.kills = None;
        row.deaths = None;
        row.assists = None;
        row.duration_seconds = None;
        row.obs_placed = None;
        row.sen_placed = None;
        row.pings = None;
        row.courier_kills = None;
        row.hero_healing = None;
        row.tower_damage = None;
        row.comeback = None;
        row.enrichment_present = false;
    }
    for row in &mut raw.year_rows {
        row.hero_id = None;
        row.kills = None;
        row.deaths = None;
        row.assists = None;
        row.duration_seconds = None;
        row.rapiers = None;
        row.enriched = false;
    }
    let slides = build_slides(raw, &BTreeMap::new(), OWNER as i64, "Player", 2026, 3, 3);
    assert!(!slides.iter().any(|slide| slide.kind == "story_identity"));
    assert!(!slides.iter().any(|slide| slide.kind == "story_superlative"));
    let summary = slides
        .iter()
        .find(|slide| slide.kind == "story_summary")
        .unwrap();
    assert!(
        summary
            .lines
            .iter()
            .any(|line| line == "No combat stats recorded.")
    );
    assert!(
        !summary
            .lines
            .iter()
            .any(|line| line.contains("0/0/0") || line.contains("0m") || line.contains("+0"))
    );
    // No recorded durations: the recap never invents match time.
    assert_eq!(
        slides.last().unwrap().lines,
        ["3|games played", "+140 JC|from gamba"]
    );
}

#[test]
fn review_hero_without_combat_does_not_claim_zero_kda() {
    let fixture = Fixture::migrated();
    Connection::open(&fixture.path).unwrap().execute("UPDATE match_participants SET kills=NULL, deaths=NULL, assists=NULL WHERE discord_id=100", []).unwrap();
    let raw = WrappedSources::new(&fixture.path)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    let slides = build_slides(raw, &BTreeMap::new(), OWNER as i64, "Player", 2026, 3, 3);
    let hero = slides
        .iter()
        .find(|slide| slide.kind == "story_hero")
        .unwrap();
    assert!(hero.lines.iter().all(|line| !line.contains("KDA")));
}

#[test]
fn review_historical_matches_survive_a_missing_registration_row() {
    let fixture = Fixture::migrated();
    let connection = Connection::open(&fixture.path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection
        .execute(
            "DELETE FROM players WHERE discord_id=100 AND guild_id=42",
            [],
        )
        .unwrap();
    let raw = WrappedSources::new(&fixture.path)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    assert_eq!(raw.matches.len(), 3);
    let slides = build_slides(
        raw,
        &BTreeMap::new(),
        OWNER as i64,
        "Known player",
        2026,
        3,
        3,
    );
    assert!(
        slides
            .iter()
            .any(|slide| slide.kind == "story_games" && slide.stat_value == "3")
    );
    assert!(
        slides
            .last()
            .unwrap()
            .lines
            .iter()
            .any(|line| line == "3|games played")
    );
}

#[test]
fn review_summary_keeps_win_rate_and_aggregate_kda() {
    let fixture = Fixture::migrated();
    let raw = WrappedSources::new(&fixture.path)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    let slides = build_slides(raw, &BTreeMap::new(), OWNER as i64, "Player", 2026, 3, 3);
    let summary = slides
        .iter()
        .find(|slide| slide.kind == "story_summary")
        .unwrap();
    assert!(summary.lines.iter().any(|line| line.contains("67%")));
    assert!(
        summary
            .lines
            .iter()
            .any(|line| line.contains("5.5 aggregate KDA") && line.contains("3 recorded games"))
    );
}

#[test]
fn personal_story_does_not_invent_a_percentile_without_a_comparison_cohort() {
    let fixture = Fixture::migrated();
    let mut raw = WrappedSources::new(&fixture.path)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    raw.player_stats.clear();
    let slides = build_slides(raw, &BTreeMap::new(), OWNER as i64, "Player", 2026, 3, 3);
    let games = slides
        .iter()
        .find(|slide| slide.kind == "story_games")
        .unwrap();
    assert_eq!(games.stat_value, "3");
    assert!(!games.lines.iter().any(|line| line.contains("percentile")));
}

#[tokio::test]
async fn review_navigation_serializes_delivery_and_rolls_back_failures() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let fixture = Fixture::migrated();
    let provider = build_provider(&fixture, FakeDiscord::production(), STORY_TIMEOUT);
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
    let session = provider
        .handler
        .sessions
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();
    // Cache the next two pages so polling tests delivery ordering, without CPU
    // scheduling or render duration affecting this regression.
    {
        let mut session = session.lock().await;
        for index in [1, 2] {
            let media = render_wrapped_media(&session.slides[index], true).unwrap();
            session.rendered.insert((false, index, true), media);
        }
    }
    let gate = Arc::new(tokio::sync::Notify::new());
    *responder.update_gate.lock().unwrap() = Some(gate.clone());
    let mut first = Box::pin(handler.handle(component(&next_id, OWNER), responder.clone()));
    let mut second = Box::pin(handler.handle(component(&next_id, OWNER), responder.clone()));
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(first.as_mut().poll(&mut context), Poll::Pending));
    assert!(matches!(second.as_mut().poll(&mut context), Poll::Pending));
    assert!(responder.updates.lock().unwrap().is_empty());
    assert_eq!(
        responder.defers.lock().unwrap().len(),
        3,
        "both clicks acknowledged before a delivery lock wait"
    );
    gate.notify_one();
    assert!(matches!(
        first.as_mut().poll(&mut context),
        Poll::Ready(Ok(()))
    ));
    assert!(matches!(
        second.as_mut().poll(&mut context),
        Poll::Ready(Ok(()))
    ));
    assert_eq!(session.lock().await.current, 2);
    assert!(
        responder
            .updates
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .content
            .contains("Chapter 3/")
    );
    responder.fail_update.store(true, AtomicOrdering::SeqCst);
    let motion_id = next_id.replace(":next", ":motion");
    assert!(
        handler
            .handle(component(&motion_id, OWNER), responder.clone())
            .await
            .is_err()
    );
    assert!(
        session.lock().await.animated,
        "failed delivery must preserve displayed mode"
    );
    assert_eq!(session.lock().await.current, 2);
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
async fn migrated_sqlite_builds_personal_story_and_preserves_detailed_stats() {
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
        assert!(
            followups[0]
                .content
                .starts_with("**Cama Wrapped 2026** for Guild Owner\nChapter 1/")
        );
        assert_native_media(&followups[0]);
        assert_eq!(followups[0].components[0].buttons.len(), 5);
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
        locked
            .slides
            .iter()
            .chain(&locked.details)
            .find(|slide| slide.kind == "server_summary")
            .unwrap()
            .lines
            .iter()
            .any(|line| line.starts_with("MOST GAMES - Guild "))
    );
    let kinds = locked
        .slides
        .iter()
        .chain(&locked.details)
        .map(|slide| slide.kind.as_str())
        .collect::<BTreeSet<_>>();
    for kind in [
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
        "story_distinct",
        "chart_rating",
        "chart_gamba",
    ] {
        assert!(kinds.contains(kind), "missing {kind}");
    }
    for kind in [
        "story_teammates",
        "story_rivals",
        "story_packages",
        "story_identity",
    ] {
        assert!(!kinds.contains(kind), "{kind} left the story");
    }
    assert_eq!(locked.slides.first().unwrap().kind, "story_intro");
    assert_eq!(locked.slides.last().unwrap().kind, "story_finale");
    for slide in locked.slides.iter().chain(&locked.details) {
        let bytes = render_wrapped_slide(slide).expect("render every native story page");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }
    let wrapped_gamba = locked
        .slides
        .iter()
        .find(|slide| slide.kind == "chart_gamba")
        .expect("live Wrapped Gamba slide");
    // The year's settled bets, plotted from zero, with the lifetime degen
    // score kept on its own clearly labeled tile.
    assert_eq!(wrapped_gamba.title, "Gamba");
    assert_eq!(wrapped_gamba.year_label, "Cama Wrapped 2026");
    assert_eq!(wrapped_gamba.stat_value, "+140 JC");
    assert_eq!(wrapped_gamba.stat_label, "across 3 gambles");
    assert_eq!(wrapped_gamba.series, [0.0, 120.0, 20.0, 140.0]);
    // A profitable year is a Stonks card ending on the curve.
    assert_eq!(wrapped_gamba.layout, WrappedLayout::TwoBeat);
    assert!(wrapped_gamba.theme.is_popoff_beat());
    let stonks = wrapped_gamba.beat.as_ref().expect("Stonks copy");
    assert_eq!(stonks.treatment, WrappedPunch::Stonks);
    assert_eq!(stonks.value, "+140 JC");
    assert_eq!(stonks.credit, "+140 JC across 3 gambles");
    assert!(stonks.punch.to_lowercase().contains("stonks"));
    let report = locked
        .slides
        .iter()
        .find(|slide| slide.kind == "story_gamba")
        .expect("degen report");
    assert_eq!(report.headline, "Your biggest wins and losses.");
    assert!(
        report
            .rows
            .iter()
            .any(|row| row.primary == "47" && row.secondary == "degen score (lifetime)")
    );
    drop(locked);
    assert!(discord.peak.load(AtomicOrdering::SeqCst) <= PROFILE_CONCURRENCY);

    handler
        .handle(component(&next_id, OWNER), responder.clone())
        .await
        .expect("navigate next");
    {
        let updates = responder.updates.lock().unwrap();
        assert_eq!(updates.len(), 1);
        assert_native_media(&updates[0]);
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
        responder.followups.lock().unwrap().last().unwrap().content,
        "Only the person who started this Wrapped can use its buttons."
    );

    let restarted = build_provider(&fixture, FakeDiscord::production(), STORY_TIMEOUT);
    let restarted_registry = build_registry(&restarted);
    let restarted_responder = Arc::new(Responder::default());
    let mut new_command = command(OWNER, Some(GUILD));
    if let InteractionRequest::Command { interaction_id, .. } = &mut new_command {
        *interaction_id = 99;
    }
    restarted_registry
        .command_handler("wrapped")
        .unwrap()
        .handle(new_command, restarted_responder.clone())
        .await
        .expect("create post-restart story");
    let restarted_next_id = restarted_responder.followups.lock().unwrap()[0].components[0].buttons
        [1]
    .custom_id
    .clone();
    assert_eq!(restarted_next_id, "wrapped:99:next");
    assert_ne!(next_id, restarted_next_id);
    let mut foreign = component(&restarted_next_id, OWNER);
    if let InteractionRequest::Component { guild_id, .. } = &mut foreign {
        *guild_id = Some(GUILD + 1);
    }
    restarted_registry
        .component_handler(&restarted_next_id)
        .unwrap()
        .handle(foreign, restarted_responder.clone())
        .await
        .unwrap();
    assert!(restarted_responder.updates.lock().unwrap().is_empty());
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

#[tokio::test(start_paused = true)]
async fn timeout_deletes_public_followup_and_late_button_is_private() {
    let fixture = Fixture::migrated();
    let provider = build_provider(
        &fixture,
        FakeDiscord::production(),
        Duration::from_secs(600),
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
    let navigator = Arc::new(Responder::default());
    handler
        .handle(component(&next_id, OWNER), navigator.clone())
        .await
        .unwrap();
    tokio::time::advance(Duration::from_secs(601)).await;
    tokio::task::yield_now().await;
    assert!(provider.handler.sessions.lock().unwrap().is_empty());
    assert_eq!(
        *navigator.deleted.lock().unwrap(),
        vec![InteractionMessageReceipt {
            message_id: 77,
            channel_id: 88,
            delivery: InteractionMessageDelivery::ChannelFallback,
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
        matches: Vec::new(),
        year_rows: Vec::new(),
        year_players: Vec::new(),
        people: PeopleYear::default(),
        rating_history: Vec::new(),
        gamba_stats: None,
        activity: ActivityData::default(),
        hero_art: BTreeMap::new(),
        art: ArtSources::for_assets(&repository_assets()),
        gold: Box::new(|_| None),
    };
    let awards = generate_awards(&raw, 3, 3);
    let titles = awards.iter().map(|award| award.title).collect::<Vec<_>>();
    assert!(titles.contains(&"Most focused hero pool"));
    assert!(titles.contains(&"Most heroes played"));
    assert!(titles.contains(&"Most games played"));
    assert!(titles.contains(&"Fewest games played"));
    assert_eq!(
        titles
            .iter()
            .filter(|title| **title == "Smallest rating swings")
            .count(),
        1
    );
    assert!(!titles.contains(&"Largest rating swings"));
    assert!(!titles.contains(&"Best betting return"));
    assert!(!titles.contains(&"Lowest betting return"));
    assert!(awards.iter().all(|award| !award.flavor.is_empty()));

    let slides = build_slides(raw, &BTreeMap::new(), 1, "1", 2026, 3, 3);
    let rendered_lines = slides
        .iter()
        .flat_map(|slide| slide.lines.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered_lines.contains("MOST GAMES - Unknown player -"));
    assert!(!rendered_lines.contains("MOST GAMES - One -"));
}

/// 2026-03-01T00:00:00Z
const MARCH_2026: i64 = 1_772_323_200;

fn seed_life_outside_the_queue(fixture: &Fixture) {
    let connection = Connection::open(&fixture.path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    for index in 0..6_i64 {
        connection
            .execute(
                "INSERT INTO dig_actions(guild_id,actor_id,action_type,depth_before,depth_after,
                     jc_delta,detail,created_at)
                 VALUES(42,100,'dig',?1,?2,2,'{}',?3)",
                params![index * 20, index * 20 + 20, MARCH_2026 + index],
            )
            .unwrap();
    }
    for won in [false, false, true] {
        connection
            .execute(
                "INSERT INTO dig_actions(guild_id,actor_id,action_type,depth_before,depth_after,
                     jc_delta,detail,created_at)
                 VALUES(42,100,'boss_fight',100,100,0,json_object('boss_id','grothak','won',json(?1)),?2)",
                params![if won { "true" } else { "false" }, MARCH_2026 + 100],
            )
            .unwrap();
    }
    for (actor, target) in [(100, 300), (100, 300), (300, 100)] {
        connection
            .execute(
                "INSERT INTO dig_actions(guild_id,actor_id,target_id,action_type,depth_before,
                     depth_after,jc_delta,detail,created_at)
                 VALUES(42,?1,?2,'sabotage',0,0,-5,'{}',?3)",
                params![actor, target, MARCH_2026 + 200],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO pets(pet_id,discord_id,guild_id,name,species,adopted_at,hatched_at,
                 adopt_fee,last_fed_at,hunger_at_last_fed,times_fed,is_active)
             VALUES(1,100,42,'Sir Fluff','common_cama',?1,?1,0,?1,80,14,1)",
            params![MARCH_2026],
        )
        .unwrap();
    for game in 1..=2_i64 {
        connection
            .execute(
                "INSERT INTO mafia_games(game_id,guild_id,game_date,phase,started_at,winner,
                     payout_per_winner,roster_size,status)
                 VALUES(?1,42,'2026-03-01','RESOLVED',?2,'MAFIA',20,4,'ACTIVE')",
                params![game, MARCH_2026 + game],
            )
            .unwrap();
        for (who, role, eliminated) in [
            (100, "MAFIA", None),
            (300, "TOWNIE", Some("NIGHT")),
            (200, "DOCTOR", None),
        ] {
            connection
                .execute(
                    "INSERT INTO mafia_players(game_id,discord_id,guild_id,role,is_alive,
                         eliminated_phase)
                     VALUES(?1,?2,42,?3,?4,?5)",
                    params![game, who, role, i64::from(eliminated.is_none()), eliminated],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO mafia_actions(game_id,guild_id,actor_id,target_id,action_type,phase,
                     day_number,created_at)
                 VALUES(?1,42,100,300,'KILL','NIGHT',1,?2)",
                params![game, MARCH_2026],
            )
            .unwrap();
    }
}

fn named(profiles: &[(i64, &str)]) -> BTreeMap<i64, WrappedDiscordProfile> {
    profiles
        .iter()
        .map(|(id, name)| (*id, profile(name)))
        .collect()
}

fn repository_assets() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets")
}

#[test]
fn top_heroes_wear_portraits_from_the_warm_cache_only() {
    let fixture = Fixture::migrated();
    let heroes = tempfile::tempdir().unwrap();
    let dotabase = heroes.path().join("dotabase.db");
    Connection::open(&dotabase)
        .unwrap()
        .execute_batch(
            "CREATE TABLE heroes (
                 id INTEGER PRIMARY KEY,name TEXT,color TEXT,localized_name TEXT,roles TEXT
             );
             INSERT INTO heroes VALUES
                 (1,'npc_dota_hero_antimage','#784094','Anti-Mage','Carry'),
                 (2,'npc_dota_hero_axe','#b32b2b','Axe','Initiator');",
        )
        .unwrap();
    let cache = heroes.path().join("trivia");
    std::fs::create_dir_all(cache.join("heroes/renders")).unwrap();
    let portrait = std::fs::read(repository_assets().join("wrapped/ward.png")).unwrap();
    std::fs::write(cache.join("heroes/antimage.png"), &portrait).unwrap();
    let render = std::fs::read(repository_assets().join("wrapped/courier.png")).unwrap();
    std::fs::write(cache.join("heroes/renders/antimage.png"), &render).unwrap();
    let art = ArtSources::for_assets(&repository_assets()).with_hero_portraits(
        cama_app::hero_lookup::HeroLookup::new(&dotabase),
        cache.clone(),
    );
    // Anti-Mage is the main hero: two of the player's three games.
    Connection::open(&fixture.path)
        .unwrap()
        .execute(
            "UPDATE match_participants SET hero_id=1 WHERE match_id=3 AND discord_id=100",
            [],
        )
        .unwrap();
    let raw = WrappedSources::with_art(&fixture.path, art)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    let slides = build_slides(raw, &BTreeMap::new(), OWNER as i64, "Player", 2026, 3, 3);
    let card = slides
        .iter()
        .find(|slide| slide.kind == "story_hero")
        .unwrap();
    let image = |name: &str| {
        card.rows
            .iter()
            .find(|row| row.primary == name)
            .unwrap()
            .image_png
            .clone()
    };
    assert_eq!(image("Anti-Mage"), Some(portrait.clone()));
    assert_eq!(card.art_png, Some(render), "the main hero's render cutout");
    let recap = slides
        .iter()
        .find(|slide| slide.kind == "story_finale")
        .unwrap();
    assert_eq!(
        recap
            .rows
            .iter()
            .find(|row| row.primary == "Anti-Mage")
            .unwrap()
            .image_png,
        Some(portrait)
    );
    // Axe was never cached, and Wrapped does not download it.
    assert_eq!(image("Axe"), None);
    assert!(!cache.join("heroes/axe.png").exists());
    assert!(render_wrapped_slide(card).is_ok());
}

#[test]
fn life_outside_the_queue_gets_its_own_chapters_and_a_cama_type() {
    let fixture = Fixture::migrated();
    seed_life_outside_the_queue(&fixture);
    let sources =
        WrappedSources::with_art(&fixture.path, ArtSources::for_assets(&repository_assets()));
    let raw = sources
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    assert_eq!(raw.activity.dig.digs, 6);
    assert_eq!(raw.activity.mafia.games, 2);
    assert!(
        raw.activity.boss_art.contains_key("encounter"),
        "the most-fought boss brings its authored scenes"
    );
    assert!(raw.activity.pet_art.is_some(), "the living pet is drawn");
    let profile_ids = prefetch_ids(&raw, OWNER as i64, 3, 3);
    assert!(
        profile_ids.contains(&300),
        "chapter subjects are prefetched"
    );
    assert!(profile_ids.contains(&(OWNER as i64)));

    let profiles = named(&[(100, "Guild Owner"), (300, "Guild Rival")]);
    let slides = build_slides(raw, &profiles, OWNER as i64, "Guild Owner", 2026, 3, 3);
    let kinds = slides
        .iter()
        .map(|slide| slide.kind.as_str())
        .filter(|kind| !is_detail_page(kind))
        .collect::<Vec<_>>();
    for kind in [
        "chart_gamba",
        "story_dig",
        "story_pet",
        "story_mafia",
        "story_persona",
    ] {
        assert!(kinds.contains(&kind), "missing {kind} in {kinds:?}");
    }
    // Four side-game cards at most: each game's main card stays, extras
    // move to More stats.
    for kind in [
        "more_story_gamba",
        "more_story_dig_boss",
        "more_story_dig_feud",
        "more_story_mafia_victim",
    ] {
        assert!(
            slides
                .iter()
                .any(|slide| slide.kind == kind && is_detail_page(&slide.kind)),
            "{kind} is under More stats"
        );
    }
    assert!(
        slides
            .iter()
            .any(|slide| slide.kind == "story_months" && is_detail_page(&slide.kind)),
        "the month-by-month chart moved to More stats"
    );
    let position = |kind| {
        kinds
            .iter()
            .position(|candidate| *candidate == kind)
            .unwrap()
    };
    assert!(position("story_hero") < position("story_dig"));
    assert!(position("story_dig") < position("story_pet"));
    assert!(position("story_pet") < position("story_mafia"));
    assert_eq!(position("story_persona"), kinds.len() - 2);
    assert_eq!(*kinds.last().unwrap(), "story_finale");

    let find = |kind| slides.iter().find(|slide| slide.kind == kind).unwrap();
    let boss = find("more_story_dig_boss");
    assert_eq!(boss.layout, WrappedLayout::Spotlight);
    assert!(boss.art_png.is_some());
    assert_eq!(boss.stat_value, "3 fights");
    let pet = find("story_pet");
    assert_eq!(pet.headline, "Sir Fluff");
    assert!(pet.art_png.is_some());
    assert_eq!(
        find("more_story_dig_feud").headline,
        "You targeted Guild Rival most often."
    );
    assert_eq!(
        find("more_story_mafia_victim").headline,
        "You targeted Guild Rival most often."
    );
    assert!(
        find("more_story_mafia_victim")
            .stat_label
            .contains("Guild Rival")
    );
    // The pet card claims only this year's spending or lifetime feedings.
    assert_eq!(pet.stat_value, "14 meals");
    assert_eq!(pet.stat_label, "served, lifetime");
    let persona = find("story_persona");
    assert!(!persona.stat_value.is_empty());
    assert_eq!(persona.title, "Your Cama type");
    assert_eq!(persona.lines, std::slice::from_ref(&persona.headline));
    // The pills are the stories behind the title, strongest first.
    assert_eq!(
        persona
            .rows
            .iter()
            .map(|row| (row.primary.as_str(), row.secondary.as_str()))
            .collect::<Vec<_>>(),
        [("1", "Divine Rapier"), ("11 / 5 / 13", "Bane")]
    );
    let recap = slides.last().unwrap();
    assert_eq!(recap.headline, persona.stat_value);
    // Games, time, then the best popoff, ahead of the side games.
    assert_eq!(recap.lines[0], "3|games played");
    assert_eq!(recap.lines[2], "1|Divine Rapier");
    assert!(recap.lines.iter().any(|line| line.ends_with("|blocks dug")));
    assert_eq!(recap.lines.len(), 5);
    for slide in &slides {
        render_wrapped_slide(slide).expect("every chapter renders");
    }
}

#[test]
fn chapters_never_name_people_without_a_readable_name() {
    let fixture = Fixture::migrated();
    seed_life_outside_the_queue(&fixture);
    let raw = WrappedSources::new(&fixture.path)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    // 300 only ever had mention markup where a name should be.
    let profiles = named(&[(100, "Guild Owner"), (300, "<@300>")]);
    let slides = build_slides(raw, &profiles, OWNER as i64, "Guild Owner", 2026, 3, 3);
    assert!(
        slides
            .iter()
            .all(|slide| !slide.kind.ends_with("story_mafia_victim"))
    );
    let feud = slides
        .iter()
        .find(|slide| slide.kind == "more_story_dig_feud")
        .unwrap();
    assert_eq!(feud.headline, "Your sabotage activity.");
    for slide in slides.iter().filter(|slide| {
        matches!(
            slide.kind.trim_start_matches("more_"),
            "story_dig" | "story_dig_feud" | "story_pet" | "story_mafia" | "story_persona"
        )
    }) {
        for text in std::iter::once(&slide.headline)
            .chain(&slide.lines)
            .chain(std::iter::once(&slide.stat_label))
        {
            assert!(!text.contains("300"), "{}: {text}", slide.kind);
            assert!(!text.contains(UNKNOWN_PLAYER), "{}: {text}", slide.kind);
        }
    }
}

#[tokio::test]
async fn recap_can_be_shared_once_as_a_lasting_message() {
    let fixture = Fixture::migrated();
    let provider = build_provider(&fixture, FakeDiscord::production(), STORY_TIMEOUT);
    let registry = build_registry(&provider);
    let handler = registry.command_handler("wrapped").unwrap();
    let responder = Arc::new(Responder::default());
    handler
        .handle(command(OWNER, Some(GUILD)), responder.clone())
        .await
        .unwrap();
    let recap_id = responder.followups.lock().unwrap()[0].components[0]
        .buttons
        .iter()
        .find(|button| button.label == "My recap")
        .unwrap()
        .custom_id
        .clone();
    handler
        .handle(component(&recap_id, OWNER), responder.clone())
        .await
        .unwrap();
    let share_id = {
        let updates = responder.updates.lock().unwrap();
        let share = updates.last().unwrap().components[0]
            .buttons
            .iter()
            .find(|button| button.label == "Share recap")
            .expect("the recap offers sharing")
            .clone();
        assert!(!share.disabled);
        share.custom_id
    };
    // Someone else cannot share another player's story.
    handler
        .handle(component(&share_id, 999), responder.clone())
        .await
        .unwrap();
    assert!(
        responder
            .followups
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .ephemeral
    );
    let posted_before = responder.followups.lock().unwrap().len();

    handler
        .handle(component(&share_id, OWNER), responder.clone())
        .await
        .unwrap();
    {
        let followups = responder.followups.lock().unwrap();
        assert_eq!(followups.len(), posted_before + 1);
        let shared = followups.last().unwrap();
        assert!(!shared.ephemeral, "the recap is posted for everyone");
        assert!(
            shared.components.is_empty(),
            "the shared card has no controls"
        );
        assert_eq!(shared.content, "**Cama Wrapped 2026** · Guild Owner");
        assert_native_png(shared);
        let updates = responder.updates.lock().unwrap();
        let button = updates.last().unwrap().components[0]
            .buttons
            .iter()
            .find(|button| button.custom_id == share_id)
            .unwrap()
            .clone();
        assert_eq!(button.label, "Shared");
        assert!(button.disabled);
    }

    handler
        .handle(component(&share_id, OWNER), responder.clone())
        .await
        .unwrap();
    let followups = responder.followups.lock().unwrap();
    assert_eq!(followups.len(), posted_before + 2);
    assert!(
        followups.last().unwrap().ephemeral,
        "a second share is refused privately"
    );
}

#[test]
fn thin_decks_borrow_personal_awards_without_fine_print() {
    let fixture = Fixture::migrated();
    let mut raw = WrappedSources::new(&fixture.path)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    for row in &mut raw.matches {
        row.pings = Some(80);
        row.obs_placed = Some(10);
        row.sen_placed = Some(10);
        row.courier_kills = Some(3);
        row.kills = Some(12);
        row.deaths = Some(0);
        row.assists = Some(30);
        row.tower_damage = Some(10_000);
        row.hero_healing = Some(12_000);
        row.duration_seconds = Some(1200);
        row.won = Some(true);
    }
    // Unparsed combat and no Rapier leave nothing distinct to rank.
    for row in &mut raw.year_rows {
        row.kills = None;
        row.rapiers = None;
    }
    let slides = build_slides(raw, &BTreeMap::new(), OWNER as i64, "Player", 2026, 3, 3);
    let story = slides
        .iter()
        .filter(|slide| !is_detail_page(&slide.kind))
        .collect::<Vec<_>>();
    assert!(story.iter().all(|slide| slide.kind != "story_distinct"));
    let awards = story
        .iter()
        .filter(|slide| slide.kind == "story_superlative")
        .collect::<Vec<_>>();
    assert_eq!(awards.len(), 3, "every personal award joins a thin deck");
    for award in &awards {
        assert!(award.badge.is_empty());
        assert!(!award.headline.is_empty());
        assert!(!award.stat_value.is_empty());
        assert!(!award.stat_label.is_empty());
        // The detail line stays; the coverage fine print does not.
        assert_eq!(award.lines.len(), 1);
        assert!(!award.lines[0].starts_with("Recorded in"));
    }
    assert_eq!(awards[0].title, "Can't touch this");
    assert_eq!(awards[1].title, "Speedrun");
    assert_eq!(awards[2].title, "Assist machine");
    assert_eq!(story.last().unwrap().badge, awards[0].title);
    // The borrowed cards sit where the distinct stories would.
    let position = |kind: &str| story.iter().position(|slide| slide.kind == kind).unwrap();
    assert!(position("story_superlative") > position("story_hero"));
    assert!(
        story
            .iter()
            .filter(|slide| slide.kind == "story_moment")
            .count()
            <= STORY_MOMENTS
    );
    let overflow = slides
        .iter()
        .filter(|slide| slide.kind == "records_moments")
        .collect::<Vec<_>>();
    assert!(!overflow.is_empty());
    for moment in overflow {
        assert_eq!(
            moment.layout,
            WrappedLayout::BigNumber,
            "detail navigation must retain the numeric evidence renderer"
        );
        assert!(!moment.stat_value.is_empty());
        assert!(!moment.stat_label.is_empty());
        assert_eq!(moment.lines.len(), 2);
        assert!(moment.lines[1].starts_with("Recorded in"));
    }
}

#[test]
fn personal_superlatives_are_absent_without_observed_match_evidence() {
    let fixture = Fixture::migrated();
    let mut raw = WrappedSources::new(&fixture.path)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    raw.matches.clear();
    let slides = build_slides(raw, &BTreeMap::new(), OWNER as i64, "Player", 2026, 3, 3);
    assert!(slides.iter().all(|slide| slide.kind != "story_superlative"));
    // No award to name: the recap wears the best popoff's fact line.
    assert_eq!(
        slides.last().unwrap().badge,
        "1 Divine Rapier in parsed games"
    );
}

#[test]
fn hero_render_reads_cache_and_never_downloads() {
    let heroes = tempfile::tempdir().unwrap();
    let dotabase = heroes.path().join("dotabase.db");
    Connection::open(&dotabase)
        .unwrap()
        .execute_batch(
            "CREATE TABLE heroes (
                 id INTEGER PRIMARY KEY,name TEXT,color TEXT,localized_name TEXT,roles TEXT
             );
             INSERT INTO heroes VALUES
                 (1,'npc_dota_hero_antimage','#784094','Anti-Mage','Carry'),
                 (2,'npc_dota_hero_axe','#b32b2b','Axe','Initiator');",
        )
        .unwrap();
    let cache = heroes.path().join("trivia");
    std::fs::create_dir_all(cache.join("heroes/renders")).unwrap();
    std::fs::write(cache.join("heroes/renders/antimage.png"), b"render").unwrap();
    std::fs::write(
        cache.join("heroes/renders/axe.png"),
        vec![0_u8; 4 * 1024 * 1024 + 1],
    )
    .unwrap();
    let art = ArtSources::for_assets(&repository_assets()).with_hero_portraits(
        cama_app::hero_lookup::HeroLookup::new(&dotabase),
        cache.clone(),
    );
    assert_eq!(art.hero_render(1), Some(b"render".to_vec()));
    // Oversized files, unknown heroes and uncached renders yield nothing.
    assert_eq!(art.hero_render(2), None);
    assert_eq!(art.hero_render(999), None);
    assert!(!cache.join("heroes/renders/999.png").exists());
}

/// Peers carry snowflake-sized ids, so a leaked id shows as a long digit run.
const PEER: i64 = 900_000_000_000_000_001;
/// On the player's team in every 2026 game.
const DUO: i64 = PEER;
/// On the other team in every 2026 game.
const NEMESIS: i64 = PEER + 1;
/// The player's partner in 2025, absent in 2026.
const OLD_DUO: i64 = PEER + 2;
/// Twelve more peers fill the remaining seats in turn.
const ROTATING_FIRST: i64 = PEER + 3;

impl Fixture {
    /// A fifteen-player 2026 league of twelve games. The player is in the
    /// first `games` of them, and won twelve 2025 games with `OLD_DUO`.
    fn league(games: i64) -> Self {
        let directory = tempfile::tempdir().expect("temporary Wrapped directory");
        let path = directory.path().join("league.sqlite3");
        initialize_or_migrate(&path).expect("migrate Wrapped database");
        let connection = Connection::open(&path).expect("open league fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("disable fixture foreign keys");
        for id in std::iter::once(OWNER as i64).chain((0..15).map(|index| PEER + index)) {
            connection
                .execute(
                    "INSERT INTO players(discord_id,guild_id,discord_username) VALUES(?1,42,?2)",
                    params![id, format!("Registered {id}")],
                )
                .expect("insert league player");
        }
        let seat = |match_id: i64, id: i64, radiant: bool, won: bool| {
            // The player stands out; everyone else differs a little.
            let (hero, kills, deaths, last_hits, gpm, damage) = if id == OWNER as i64 {
                (1, 22, 13, 380, 820, 61_000)
            } else {
                let offset = id - PEER;
                (
                    2 + offset,
                    4 + offset % 5,
                    4 + offset % 4,
                    120 + offset * 5,
                    380 + offset * 7,
                    14_000 + offset * 300,
                )
            };
            connection
                .execute(
                    "INSERT INTO match_participants(
                         match_id,discord_id,guild_id,team_number,side,won,hero_id,
                         kills,deaths,assists,last_hits,gpm,hero_damage
                     ) VALUES(?1,?2,42,?3,?4,?5,?6,?7,?8,10,?9,?10,?11)",
                    params![
                        match_id,
                        id,
                        if radiant { 1 } else { 2 },
                        if radiant { "radiant" } else { "dire" },
                        won,
                        hero,
                        kills,
                        deaths,
                        last_hits,
                        gpm,
                        damage,
                    ],
                )
                .expect("insert league participant");
        };
        let game = |match_id: i64, date: String, radiant_won: bool, gold: Option<&str>| {
            connection
                .execute(
                    "INSERT INTO matches(
                         match_id,team1_players,team2_players,winning_team,match_date,
                         guild_id,duration_seconds,enrichment_data
                     ) VALUES(?1,'[]','[]',?2,?3,42,2400,?4)",
                    params![match_id, if radiant_won { 1 } else { 2 }, date, gold],
                )
                .expect("insert league match");
        };
        for index in 0..12_i64 {
            let match_id = index + 1;
            let radiant_won = index % 6 != 5;
            // The sixth game is the one the player's team threw.
            let gold = (index == 5)
                .then_some(r#"{"radiant_gold_adv":[0,1500,6000,12000,15000,9000,2000,-4000]}"#);
            // 08:30 UTC finishes are the small hours in New York.
            game(
                match_id,
                format!("2026-03-{:02} 08:30:00", index + 1),
                radiant_won,
                gold,
            );
            let mut radiant = vec![DUO];
            if index < games {
                radiant.push(OWNER as i64);
            }
            let mut dire = vec![NEMESIS];
            for chair in 0..7 {
                let peer = ROTATING_FIRST + (index * 7 + chair) % 12;
                if chair < 3 {
                    radiant.push(peer);
                } else {
                    dire.push(peer);
                }
            }
            for id in radiant {
                seat(match_id, id, true, radiant_won);
            }
            for id in dire {
                seat(match_id, id, false, !radiant_won);
            }
        }
        if games > 5 {
            connection
                .execute(
                    "INSERT INTO wrapped_enrichment_facts(
                         guild_id,match_id,discord_id,rapier_count,comeback,throw
                     ) VALUES(42,6,100,0,0,15000)",
                    [],
                )
                .expect("insert the thrown game's facts");
        }
        if games > 2 {
            connection
                .execute(
                    "INSERT INTO rating_history(
                         discord_id,guild_id,rating_before,rating,timestamp,match_id,
                         expected_team_win_prob
                     ) VALUES(100,42,1500,1530,'2026-03-03 09:00:00',3,0.18)",
                    [],
                )
                .expect("insert the upset win");
        }
        for index in 0..12_i64 {
            let match_id = 1001 + index;
            game(
                match_id,
                format!("2025-06-{:02} 20:00:00", index + 1),
                true,
                None,
            );
            seat(match_id, OWNER as i64, true, true);
            seat(match_id, OLD_DUO, true, true);
            seat(match_id, ROTATING_FIRST, false, false);
        }
        Self {
            _directory: directory,
            path,
        }
    }
}

fn league_names() -> BTreeMap<i64, WrappedDiscordProfile> {
    named(&[
        (OWNER as i64, "Guild Owner"),
        (DUO, "Duo Partner"),
        (NEMESIS, "Arch Rival"),
        (OLD_DUO, "Old Partner"),
        // The side games' rival from `seed_life_outside_the_queue`.
        (300, "Guild Rival"),
    ])
}

fn league_deck(
    fixture: &Fixture,
    profiles: &BTreeMap<i64, WrappedDiscordProfile>,
) -> Vec<WrappedSlideData> {
    let raw = WrappedSources::new(&fixture.path)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    build_slides(raw, profiles, OWNER as i64, "Guild Owner", 2026, 3, 3)
}

/// Every piece of copy a card can show.
fn slide_texts(slide: &WrappedSlideData) -> Vec<String> {
    let mut texts = vec![
        slide.kind.clone(),
        slide.title.clone(),
        slide.headline.clone(),
        slide.stat_value.clone(),
        slide.stat_label.clone(),
        slide.badge.clone(),
    ];
    texts.extend(slide.lines.iter().cloned());
    texts.extend(
        slide
            .rows
            .iter()
            .flat_map(|row| [row.primary.clone(), row.secondary.clone()]),
    );
    if let Some(beat) = &slide.beat {
        texts.extend([
            beat.setup.clone(),
            beat.value.clone(),
            beat.setup_label.clone(),
            beat.punch.clone(),
            beat.credit.clone(),
        ]);
    }
    texts
}

#[test]
fn deck_has_two_beat_distinct_cards_for_an_active_player() {
    let fixture = Fixture::league(12);
    let slides = league_deck(&fixture, &league_names());
    let distinct = slides
        .iter()
        .filter(|slide| slide.kind == "story_distinct")
        .collect::<Vec<_>>();
    assert!(
        (5..=8).contains(&distinct.len()),
        "{} distinct cards",
        distinct.len()
    );
    for slide in &distinct {
        assert_eq!(slide.layout, WrappedLayout::TwoBeat);
        let beat = slide.beat.as_ref().expect("two-beat copy");
        assert!(!beat.setup.is_empty(), "{beat:?}");
        assert!(!beat.punch.is_empty(), "{beat:?}");
        assert!(!beat.credit.is_empty(), "{beat:?}");
        assert!(slide.theme.is_roast_beat() || slide.theme.is_popoff_beat());
        render_wrapped_slide(slide).expect("every distinct card renders");
    }
    // The thrown game plots its gold lead from the player's side.
    let graph = distinct
        .iter()
        .find(|slide| {
            slide
                .beat
                .as_ref()
                .is_some_and(|beat| beat.treatment == WrappedPunch::GoldGraph)
        })
        .expect("the thrown game becomes a gold graph");
    assert_eq!(
        graph.series,
        [
            0.0, 1500.0, 6000.0, 12000.0, 15000.0, 9000.0, 2000.0, -4000.0
        ]
    );
    // Two roasts and two popoffs at least, and the recap carries the best
    // of each.
    let roasts = distinct
        .iter()
        .filter(|slide| slide.theme.is_roast_beat())
        .count();
    assert!(roasts >= 2 && distinct.len() - roasts >= 2);
    let recap = slides.last().unwrap();
    assert_eq!(recap.kind, "story_finale");
    assert!(
        recap.lines.contains(&"22 / 13 / 10|Anti-Mage".to_owned()),
        "{:?}",
        recap.lines
    );
    assert!(
        recap.lines.contains(&"13.0|deaths a game".to_owned()),
        "{:?}",
        recap.lines
    );
    let story = slides
        .iter()
        .filter(|slide| !is_detail_page(&slide.kind))
        .map(|slide| slide.kind.as_str())
        .collect::<Vec<_>>();
    let position = |wanted: &str| story.iter().position(|kind| *kind == wanted).unwrap();
    assert!(position("story_distinct") > position("story_hero"));
    assert!(
        story
            .iter()
            .all(|kind| !matches!(*kind, "story_moment" | "story_superlative"))
    );
}

#[test]
fn deck_contains_no_package_deal_or_moderation_content() {
    let fixture = Fixture::migrated();
    Connection::open(&fixture.path)
        .unwrap()
        .execute(
            "INSERT INTO low_priority_state(
                 discord_id,guild_id,wins_required,wins_remaining,active,set_by
             ) VALUES(100,42,3,3,1,400)",
            [],
        )
        .unwrap();
    let raw = WrappedSources::new(&fixture.path)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    let slides = build_slides(raw, &BTreeMap::new(), OWNER as i64, "Player", 2026, 3, 3);
    for slide in &slides {
        assert!(!slide.kind.contains("package"), "{}", slide.kind);
        for text in slide_texts(slide) {
            let lower = text.to_lowercase();
            for banned in ["package deal", "low priority", "low-priority"] {
                assert!(!lower.contains(banned), "{}: {text}", slide.kind);
            }
        }
    }
}

#[test]
fn people_cards_are_year_scoped() {
    let fixture = Fixture::league(12);
    let slides = league_deck(&fixture, &league_names());
    let people = slides
        .iter()
        .filter(|slide| slide.kind == "story_people")
        .collect::<Vec<_>>();
    assert!(
        !people.is_empty(),
        "the league has year-scoped people cards"
    );
    let duo = people
        .iter()
        .find(|slide| slide.title == "Your duo")
        .expect("a duo card");
    let beat = duo.beat.as_ref().expect("two-beat duo copy");
    assert_eq!(beat.value, "10");
    assert!(beat.credit.contains("Duo Partner"), "{beat:?}");
    for slide in &people {
        assert_eq!(slide.layout, WrappedLayout::TwoBeat);
        for text in slide_texts(slide) {
            assert!(!text.contains("Old Partner"), "{text}");
        }
    }
}

#[test]
fn people_cards_never_show_ids_when_names_are_missing() {
    let fixture = Fixture::league(12);
    // The duo partner's only known name is the placeholder; nobody else has one.
    let slides = league_deck(&fixture, &named(&[(DUO, UNKNOWN_PLAYER)]));
    let people = slides
        .iter()
        .filter(|slide| slide.kind == "story_people")
        .collect::<Vec<_>>();
    assert!(!people.is_empty(), "people cards survive missing names");
    for slide in people {
        let beat = slide.beat.as_ref().expect("two-beat people copy");
        assert!(!beat.setup.is_empty() && !beat.punch.is_empty());
        for text in slide_texts(slide) {
            let longest_digits = text
                .split(|character: char| !character.is_ascii_digit())
                .map(str::len)
                .max()
                .unwrap_or(0);
            assert!(longest_digits < 5, "{text}");
            assert!(!text.contains("<@"), "{text}");
            assert!(!text.contains(UNKNOWN_PLAYER), "{text}");
        }
    }
}

#[test]
fn below_minimum_target_falls_back() {
    let fixture = Fixture::league(2);
    // A long, deathless win gives the fallback something to say.
    let connection = Connection::open(&fixture.path).unwrap();
    connection
        .execute(
            "UPDATE matches SET duration_seconds=3700 WHERE match_id=1",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE match_participants SET deaths=0 WHERE match_id=1 AND discord_id=100",
            [],
        )
        .unwrap();
    let slides = league_deck(&fixture, &league_names());
    let story = slides
        .iter()
        .filter(|slide| !is_detail_page(&slide.kind))
        .collect::<Vec<_>>();
    assert!(story.iter().all(|slide| slide.kind != "story_distinct"));
    assert!(
        story
            .iter()
            .any(|slide| matches!(slide.kind.as_str(), "story_moment" | "story_superlative")),
        "{:?}",
        story.iter().map(|slide| &slide.kind).collect::<Vec<_>>()
    );
    for slide in &slides {
        render_wrapped_slide(slide).expect("a thin deck still renders");
    }
}

#[test]
fn story_cards_have_no_coverage_fine_print() {
    let thin = Fixture::league(2);
    Connection::open(&thin.path)
        .unwrap()
        .execute(
            "UPDATE matches SET duration_seconds=3700 WHERE match_id=1",
            [],
        )
        .unwrap();
    let decks = [
        league_deck(&thin, &league_names()),
        league_deck(&Fixture::league(12), &league_names()),
        {
            let fixture = Fixture::migrated();
            let raw = WrappedSources::new(&fixture.path)
                .load(GUILD as i64, OWNER as i64, 2026)
                .unwrap()
                .unwrap();
            build_slides(raw, &BTreeMap::new(), OWNER as i64, "Player", 2026, 3, 3)
        },
    ];
    assert!(
        decks[0].iter().any(|slide| slide.kind == "story_playtime"),
        "the thin deck keeps its time card"
    );
    for slide in decks
        .iter()
        .flatten()
        .filter(|slide| !is_detail_page(&slide.kind))
    {
        for text in slide_texts(slide) {
            assert!(!text.starts_with("Recorded in"), "{}: {text}", slide.kind);
        }
    }
    // The evidence stays on the detail pages.
    let overflow = decks[1]
        .iter()
        .filter(|slide| slide.kind == "records_moments")
        .collect::<Vec<_>>();
    assert!(!overflow.is_empty());
    assert!(overflow.iter().all(|slide| {
        slide
            .lines
            .iter()
            .any(|line| line.starts_with("Recorded in"))
    }));
}

#[tokio::test]
async fn deck_is_still_public_and_acknowledged_first() {
    let fixture = Fixture::league(12);
    let provider = build_provider(&fixture, FakeDiscord::production(), STORY_TIMEOUT);
    let registry = build_registry(&provider);
    let responder = Arc::new(Responder::default());
    registry
        .command_handler("wrapped")
        .unwrap()
        .handle(command(OWNER, Some(GUILD)), responder.clone())
        .await
        .unwrap();
    assert_eq!(*responder.defers.lock().unwrap(), vec![false]);
    {
        let followups = responder.followups.lock().unwrap();
        assert_eq!(followups.len(), 1);
        assert!(!followups[0].ephemeral);
        assert_native_media(&followups[0]);
    }
    let session = provider
        .handler
        .sessions
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();
    let session = session.lock().await;
    assert!(
        session
            .slides
            .iter()
            .any(|slide| slide.kind == "story_distinct")
    );
    assert_eq!(session.slides.last().unwrap().kind, "story_finale");
}

/// The league plus every side game: Dig with cave-ins, a boss and a feud,
/// a living pet and a lost one, Mafia, and a profitable gamba year.
fn seed_all_activity(fixture: &Fixture) {
    seed_life_outside_the_queue(fixture);
    let connection = Connection::open(&fixture.path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    for (match_id, payout) in [(1_i64, Some(260_i64)), (2, None), (3, None), (4, None)] {
        connection
            .execute(
                "INSERT INTO bets(guild_id,match_id,discord_id,team_bet_on,amount,leverage,
                     payout,bet_time,is_blind)
                 VALUES(42,?1,100,'radiant',100,1,?2,?3,0)",
                params![match_id, payout, MARCH_2026 + match_id],
            )
            .unwrap();
    }
    for index in 0..4_i64 {
        connection
            .execute(
                "INSERT INTO dig_actions(guild_id,actor_id,action_type,depth_before,depth_after,
                     jc_delta,detail,created_at)
                 VALUES(42,100,'dig',120,120,0,json_object('cave_in',1),?1)",
                params![MARCH_2026 + 500 + index],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO pets(pet_id,discord_id,guild_id,name,species,adopted_at,hatched_at,
                 adopt_fee,last_fed_at,hunger_at_last_fed,times_fed,is_active,died_at,
                 death_cause)
             VALUES(2,100,42,'Old Timer','common_cama',?1,?1,0,?1,0,3,0,?2,'starvation')",
            params![MARCH_2026 - 1_000, MARCH_2026 + 1_000],
        )
        .unwrap();
}

fn story_of(slides: &[WrappedSlideData]) -> Vec<&WrappedSlideData> {
    slides
        .iter()
        .filter(|slide| !is_detail_page(&slide.kind))
        .collect()
}

const SIDE_KINDS: [&str; 10] = [
    "chart_gamba",
    "story_gamba",
    "story_dig",
    "story_dig_boss",
    "story_dig_feud",
    "story_dig_cave_in",
    "story_pet",
    "story_pet_grave",
    "story_mafia",
    "story_mafia_victim",
];

/// Standout games for the player, all Anti-Mage wins: 31 kills on March 4,
/// which is also the best game the Cinema card shows, a 78:12 game on
/// March 7, and 1,400 GPM on March 9.
fn seed_record_games(fixture: &Fixture) {
    Connection::open(&fixture.path)
        .unwrap()
        .execute_batch(
            "UPDATE match_participants SET kills=31 WHERE match_id=4 AND discord_id=100;
             UPDATE match_participants SET gpm=1400 WHERE match_id=9 AND discord_id=100;
             UPDATE matches SET duration_seconds=4692 WHERE match_id=7;",
        )
        .unwrap();
}

/// Art sources whose warm cache holds one Anti-Mage render, and its bytes.
fn anti_mage_art() -> (TempDir, ArtSources, Vec<u8>) {
    let heroes = tempfile::tempdir().unwrap();
    let dotabase = heroes.path().join("dotabase.db");
    Connection::open(&dotabase)
        .unwrap()
        .execute_batch(
            "CREATE TABLE heroes (
                 id INTEGER PRIMARY KEY,name TEXT,color TEXT,localized_name TEXT,roles TEXT
             );
             INSERT INTO heroes VALUES (1,'npc_dota_hero_antimage','#784094','Anti-Mage','Carry');",
        )
        .unwrap();
    let cache = heroes.path().join("trivia");
    std::fs::create_dir_all(cache.join("heroes/renders")).unwrap();
    let render = std::fs::read(repository_assets().join("wrapped/courier.png")).unwrap();
    std::fs::write(cache.join("heroes/renders/antimage.png"), &render).unwrap();
    let art = ArtSources::for_assets(&repository_assets())
        .with_hero_portraits(cama_app::hero_lookup::HeroLookup::new(&dotabase), cache);
    (heroes, art, render)
}

#[test]
fn deck_respects_twenty_card_budget_for_an_all_activity_player() {
    let fixture = Fixture::league(12);
    seed_all_activity(&fixture);
    seed_record_games(&fixture);
    let slides = league_deck(&fixture, &league_names());
    let story = story_of(&slides);
    let count = |kind: &str| story.iter().filter(|slide| slide.kind == kind).count();
    assert_eq!(story.last().unwrap().kind, "story_finale");
    assert!(
        story.len() - 1 <= 20,
        "{} cards before the recap",
        story.len() - 1
    );
    for kind in ["story_intro", "story_games", "story_playtime", "story_hero"] {
        assert_eq!(count(kind), 1, "{kind}");
    }
    assert!((5..=8).contains(&count("story_distinct")));
    // The player's own records and their best hero make the cut.
    assert_eq!(count("story_record"), 2);
    assert_eq!(count("story_hero_year"), 1);
    assert!(count("story_people") <= 2);
    assert_eq!(count("story_persona"), 1);
    // Every side game's main card stays; extras drop to More stats first.
    let side = story
        .iter()
        .filter(|slide| SIDE_KINDS.contains(&slide.kind.as_str()))
        .map(|slide| slide.kind.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        side,
        ["chart_gamba", "story_dig", "story_pet", "story_mafia"]
    );
    for kind in [
        "more_story_gamba",
        "more_story_dig_boss",
        "more_story_dig_feud",
        "more_story_dig_cave_in",
        "more_story_pet_grave",
        "more_story_mafia_victim",
    ] {
        assert!(
            slides
                .iter()
                .any(|slide| slide.kind == kind && is_detail_page(&slide.kind)),
            "{kind} is under More stats"
        );
    }
}

#[test]
fn record_cards_use_hero_render_and_credit_the_game() {
    let fixture = Fixture::league(12);
    seed_record_games(&fixture);
    let (_heroes, art, render) = anti_mage_art();
    let raw = WrappedSources::with_art(&fixture.path, art)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    let slides = build_slides(
        raw,
        &league_names(),
        OWNER as i64,
        "Guild Owner",
        2026,
        3,
        3,
    );
    let story = story_of(&slides);
    let records = story
        .iter()
        .filter(|slide| slide.kind == "story_record")
        .collect::<Vec<_>>();
    // The strongest record of each kind: a roast and a popoff, each naming
    // its hero, the day it finished in the player's time zone, and the result.
    // The 31-kill game is already the Cinema card, so it is not repeated.
    let beats = records
        .iter()
        .map(|slide| slide.beat.as_ref().expect("two-beat record copy"))
        .collect::<Vec<_>>();
    assert_eq!(
        beats
            .iter()
            .map(|beat| beat.credit.as_str())
            .collect::<Vec<_>>(),
        [
            "78:12 · Anti-Mage · Mar 7 · Won",
            "1,400 GPM · Anti-Mage · Mar 9 · Won"
        ]
    );
    assert_eq!(
        (beats[1].value.as_str(), beats[1].setup_label.as_str()),
        ("1,400 GPM", "Anti-Mage · Mar 9")
    );
    let cinema = story
        .iter()
        .filter_map(|slide| slide.beat.as_ref())
        .find(|beat| beat.treatment == WrappedPunch::Cinema)
        .expect("the best game's cinema card");
    assert!(
        cinema.credit.starts_with("31 / 13 / 10"),
        "{}",
        cinema.credit
    );
    assert!(records[0].theme.is_roast_beat() && records[1].theme.is_popoff_beat());
    for (slide, beat) in records.iter().zip(&beats) {
        assert_eq!(slide.layout, WrappedLayout::TwoBeat);
        assert_eq!(slide.art_png.as_deref(), Some(render.as_slice()));
        assert_eq!(beat.treatment, WrappedPunch::Plain);
        assert!(!beat.setup.is_empty() && !beat.punch.is_empty());
        render_wrapped_slide(slide).expect("a record card renders");
    }
    // The best hero wears its render too.
    let hero = story
        .iter()
        .find(|slide| slide.kind == "story_hero_year")
        .expect("the best-hero card");
    let beat = hero.beat.as_ref().expect("two-beat hero copy");
    assert_eq!(
        (beat.value.as_str(), beat.credit.as_str()),
        ("83%", "Won 10 of 12 on Anti-Mage")
    );
    assert_eq!(hero.art_png.as_deref(), Some(render.as_slice()));
}

#[test]
fn dota_cards_come_before_side_games() {
    let fixture = Fixture::league(12);
    seed_all_activity(&fixture);
    seed_record_games(&fixture);
    let slides = league_deck(&fixture, &league_names());
    let kinds = story_of(&slides)
        .iter()
        .map(|slide| slide.kind.as_str())
        .collect::<Vec<_>>();
    let span = |kind: &str| {
        let first = kinds.iter().position(|seen| *seen == kind);
        let last = kinds.iter().rposition(|seen| *seen == kind);
        first
            .zip(last)
            .unwrap_or_else(|| panic!("{kind} in {kinds:?}"))
    };
    // Distinct stories, then records, then hero stories, then people.
    let sections = [
        "story_hero",
        "story_distinct",
        "story_record",
        "story_hero_year",
        "story_people",
    ]
    .map(span);
    for pair in sections.windows(2) {
        assert!(pair[0].1 < pair[1].0, "{kinds:?}");
    }
    let first_side = kinds
        .iter()
        .position(|kind| SIDE_KINDS.contains(kind))
        .expect("side games in the story");
    assert!(sections[4].1 < first_side, "{kinds:?}");
    assert!(
        kinds[first_side..]
            .iter()
            .all(|kind| SIDE_KINDS.contains(kind)
                || matches!(*kind, "story_persona" | "story_finale")),
        "{kinds:?}"
    );
}

#[test]
fn a_dota_only_deck_rolls_spare_slots_into_distinct_stories() {
    let fixture = Fixture::league(12);
    let slides = league_deck(&fixture, &league_names());
    let story = story_of(&slides);
    // Twelve candidates and no side games: all eight distinct slots fill.
    assert_eq!(
        story
            .iter()
            .filter(|slide| slide.kind == "story_distinct")
            .count(),
        8
    );
    assert!(story.len() - 1 <= 20);
}

#[test]
fn recap_badge_names_something_the_deck_showed() {
    let full = league_deck(&Fixture::league(12), &league_names());
    let credits = story_of(&full)
        .into_iter()
        .filter(|slide| slide.kind == "story_distinct" && slide.theme.is_popoff_beat())
        .filter_map(|slide| slide.beat.as_ref().map(|beat| beat.credit.clone()))
        .collect::<Vec<_>>();
    let badge = &full.last().unwrap().badge;
    assert_eq!(badge, "22 / 13 / 10 · Anti-Mage · #1 of 15");
    assert!(credits.contains(badge), "{badge} is a popoff in the deck");

    let thin = Fixture::league(2);
    let connection = Connection::open(&thin.path).unwrap();
    connection
        .execute(
            "UPDATE matches SET duration_seconds=3700 WHERE match_id=1",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE match_participants SET deaths=0 WHERE match_id=1 AND discord_id=100",
            [],
        )
        .unwrap();
    let slides = league_deck(&thin, &league_names());
    let award = story_of(&slides)
        .into_iter()
        .find(|slide| slide.kind == "story_superlative")
        .expect("a thin deck shows its awards")
        .title
        .clone();
    assert_eq!(slides.last().unwrap().badge, award);
}

#[test]
fn people_cards_never_call_a_beaten_opponent_a_nemesis() {
    let slides = league_deck(&Fixture::league(12), &league_names());
    let people = story_of(&slides)
        .into_iter()
        .filter(|slide| slide.kind == "story_people")
        .collect::<Vec<_>>();
    // The player won 10 of 12 against Arch Rival.
    assert!(people.iter().all(|slide| slide.title != "Your nemesis"));
    let bag = people
        .iter()
        .find(|slide| slide.title == "Punching bag")
        .expect("the opponent the player beat");
    let beat = bag.beat.as_ref().unwrap();
    assert_eq!(beat.credit, "Won 10 of 12 against Arch Rival");
}

#[test]
fn neighboring_story_cards_never_share_a_background() {
    let all = Fixture::league(12);
    seed_all_activity(&all);
    seed_record_games(&all);
    let thin = Fixture::league(2);
    Connection::open(&thin.path)
        .unwrap()
        .execute(
            "UPDATE matches SET duration_seconds=3700 WHERE match_id=1",
            [],
        )
        .unwrap();
    let default = Fixture::migrated();
    seed_life_outside_the_queue(&default);
    let decks = [
        league_deck(&all, &league_names()),
        league_deck(&Fixture::league(12), &league_names()),
        league_deck(&thin, &league_names()),
        {
            let raw = WrappedSources::new(&default.path)
                .load(GUILD as i64, OWNER as i64, 2026)
                .unwrap()
                .unwrap();
            build_slides(raw, &BTreeMap::new(), OWNER as i64, "Player", 2026, 3, 3)
        },
    ];
    for deck in &decks {
        let story = story_of(deck);
        let sequence = story
            .iter()
            .map(|slide| {
                let treatment = slide.beat.as_ref().map(|beat| beat.treatment);
                format!("{} {:?} {treatment:?}", slide.kind, slide.theme)
            })
            .collect::<Vec<_>>();
        for pair in story.windows(2) {
            let (before, after) = (slide_backgrounds(pair[0]), slide_backgrounds(pair[1]));
            assert!(
                before
                    .iter()
                    .all(|color| after.iter().all(|other| !backgrounds_alike(*color, *other))),
                "{} ({:?}) then {} ({:?}) look alike in {sequence:#?}",
                pair[0].kind,
                pair[0].theme,
                pair[1].kind,
                pair[1].theme
            );
        }
    }
}

#[test]
fn hero_stories_wear_the_hero_render() {
    let fixture = Fixture::league(12);
    let (_heroes, art, render) = anti_mage_art();
    let raw = WrappedSources::with_art(&fixture.path, art)
        .load(GUILD as i64, OWNER as i64, 2026)
        .unwrap()
        .unwrap();
    let slides = build_slides(
        raw,
        &league_names(),
        OWNER as i64,
        "Guild Owner",
        2026,
        3,
        3,
    );
    let distinct = slides
        .iter()
        .filter(|slide| slide.kind == "story_distinct")
        .collect::<Vec<_>>();
    // The best game, the latest finish, and the thrown game all name the
    // player's Anti-Mage; average stories have no single hero.
    let with_art = distinct
        .iter()
        .filter(|slide| slide.art_png.as_deref() == Some(render.as_slice()))
        .count();
    assert!(with_art >= 3, "{with_art} hero stories with art");
    assert!(
        distinct
            .iter()
            .filter(|slide| slide.art_png.is_some())
            .all(|slide| slide.art_png.as_deref() == Some(render.as_slice()))
    );
    let clock = distinct
        .iter()
        .find(|slide| {
            slide.art_png.is_some()
                && slide
                    .beat
                    .as_ref()
                    .is_some_and(|beat| beat.treatment == WrappedPunch::Clock)
        })
        .expect("the latest finish wears its hero");
    assert!(render_wrapped_slide(clock).is_ok());
}
