use std::convert::Infallible;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Barrier, Condvar};
use std::thread;
use std::time::Duration;

use super::*;

const USER: UserId = UserId(100);
const STEAM: SteamId = SteamId(12_345);

#[derive(Clone, Debug)]
struct FakePlayer {
    steam_id: Arc<Mutex<Option<SteamId>>>,
    calls: Arc<AtomicUsize>,
}

impl FakePlayer {
    fn new(steam_id: Option<SteamId>) -> Self {
        Self {
            steam_id: Arc::new(Mutex::new(steam_id)),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl PlayerSteamIdPort for FakePlayer {
    type Error = Infallible;

    fn steam_id(&self, _discord_id: UserId) -> Result<Option<SteamId>, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(*self.steam_id.lock().expect("fake player lock"))
    }
}

#[derive(Clone, Debug)]
struct FakeClock(Arc<AtomicI64>);

impl FakeClock {
    fn new(now: i64) -> Self {
        Self(Arc::new(AtomicI64::new(now)))
    }

    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl OpenDotaPlayerClock for FakeClock {
    fn now_seconds(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
struct ConcurrencyProbe {
    barrier: Barrier,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl ConcurrencyProbe {
    fn new(parties: usize) -> Self {
        Self {
            barrier: Barrier::new(parties),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        }
    }

    fn rendezvous(&self) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.barrier.wait();
        self.active.fetch_sub(1, Ordering::SeqCst);
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Default)]
struct BlockingGate {
    started: (Mutex<bool>, Condvar),
    released: (Mutex<bool>, Condvar),
}

impl BlockingGate {
    fn block(&self) {
        let (started_lock, started_ready) = &self.started;
        *started_lock.lock().expect("started lock") = true;
        started_ready.notify_all();
        let (released_lock, released_ready) = &self.released;
        let mut released = released_lock.lock().expect("released lock");
        while !*released {
            released = released_ready.wait(released).expect("released wait");
        }
    }

    fn wait_started(&self) {
        let (lock, ready) = &self.started;
        let mut started = lock.lock().expect("started lock");
        while !*started {
            started = ready.wait(started).expect("started wait");
        }
    }

    fn release(&self) {
        let (lock, ready) = &self.released;
        *lock.lock().expect("released lock") = true;
        ready.notify_all();
    }
}

#[derive(Clone, Debug)]
struct FakeApi {
    state: Arc<Mutex<FakeApiState>>,
    identity_calls: Arc<AtomicUsize>,
    win_loss_calls: Arc<AtomicUsize>,
    average_calls: Arc<AtomicUsize>,
    hero_calls: Arc<AtomicUsize>,
    projected_calls: Arc<AtomicUsize>,
    probe: Option<Arc<ConcurrencyProbe>>,
    identity_gate: Option<Arc<BlockingGate>>,
}

#[derive(Clone, Debug)]
struct FakeApiState {
    identity: Option<PlayerIdentity>,
    win_loss: WinLoss,
    averages: ProfileAverages,
    top_heroes: Vec<TopHero>,
    matches: Vec<ProjectedMatch>,
}

impl Default for FakeApi {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeApiState {
                identity: Some(PlayerIdentity {
                    persona_name: Some("TestPlayer".to_owned()),
                    avatar: None,
                    rank_tier: Some(55),
                    mmr_estimate: Some(4_500),
                }),
                win_loss: WinLoss {
                    wins: 100,
                    losses: 50,
                },
                averages: ProfileAverages {
                    kills: 8.5,
                    deaths: 5.2,
                    assists: 12.0,
                    gpm: 500,
                    xpm: 550,
                    last_hits: 150,
                },
                top_heroes: vec![TopHero {
                    hero_id: 14,
                    hero_name: "Pudge".to_owned(),
                    games: 50,
                    wins: 30,
                    win_rate: 60.0,
                }],
                matches: vec![match_row(14, Some(1), 0, true)],
            })),
            identity_calls: Arc::new(AtomicUsize::new(0)),
            win_loss_calls: Arc::new(AtomicUsize::new(0)),
            average_calls: Arc::new(AtomicUsize::new(0)),
            hero_calls: Arc::new(AtomicUsize::new(0)),
            projected_calls: Arc::new(AtomicUsize::new(0)),
            probe: None,
            identity_gate: None,
        }
    }
}

impl FakeApi {
    fn with_probe(mut self, parties: usize) -> (Self, Arc<ConcurrencyProbe>) {
        let probe = Arc::new(ConcurrencyProbe::new(parties));
        self.probe = Some(Arc::clone(&probe));
        (self, probe)
    }

    fn with_identity_gate(mut self) -> (Self, Arc<BlockingGate>) {
        let gate = Arc::new(BlockingGate::default());
        self.identity_gate = Some(Arc::clone(&gate));
        (self, gate)
    }

    fn set_wins(&self, wins: i64) {
        self.state.lock().expect("fake API lock").win_loss.wins = wins;
    }

    fn set_identity(&self, identity: Option<PlayerIdentity>) {
        self.state.lock().expect("fake API lock").identity = identity;
    }

    fn set_matches(&self, matches: Vec<ProjectedMatch>) {
        self.state.lock().expect("fake API lock").matches = matches;
    }

    fn component_calls(&self) -> usize {
        self.identity_calls.load(Ordering::SeqCst)
    }

    fn projected_calls(&self) -> usize {
        self.projected_calls.load(Ordering::SeqCst)
    }

    fn rendezvous(&self) {
        if let Some(probe) = &self.probe {
            probe.rendezvous();
        }
    }
}

impl OpenDotaPlayerApiPort for FakeApi {
    fn player_identity(
        &self,
        _steam_id: SteamId,
    ) -> Result<Option<PlayerIdentity>, OpenDotaPlayerPortError> {
        self.identity_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.identity_gate {
            gate.block();
        }
        self.rendezvous();
        Ok(self.state.lock().expect("fake API lock").identity.clone())
    }

    fn win_loss(&self, _steam_id: SteamId) -> Result<WinLoss, OpenDotaPlayerPortError> {
        self.win_loss_calls.fetch_add(1, Ordering::SeqCst);
        self.rendezvous();
        Ok(self.state.lock().expect("fake API lock").win_loss)
    }

    fn profile_averages(
        &self,
        _steam_id: SteamId,
    ) -> Result<ProfileAverages, OpenDotaPlayerPortError> {
        self.average_calls.fetch_add(1, Ordering::SeqCst);
        self.rendezvous();
        Ok(self.state.lock().expect("fake API lock").averages)
    }

    fn top_heroes(&self, _steam_id: SteamId) -> Result<Vec<TopHero>, OpenDotaPlayerPortError> {
        self.hero_calls.fetch_add(1, Ordering::SeqCst);
        self.rendezvous();
        Ok(self.state.lock().expect("fake API lock").top_heroes.clone())
    }

    fn projected_matches(
        &self,
        _steam_id: SteamId,
        _limit: usize,
    ) -> Result<Vec<ProjectedMatch>, OpenDotaPlayerPortError> {
        self.projected_calls.fetch_add(1, Ordering::SeqCst);
        self.rendezvous();
        Ok(self.state.lock().expect("fake API lock").matches.clone())
    }
}

type TestService = OpenDotaPlayerService<FakePlayer, FakeApi, FakeClock, RecordedHeroCatalog>;

fn service(player: FakePlayer, api: FakeApi, clock: FakeClock) -> TestService {
    OpenDotaPlayerService::new(player, api, clock, catalog())
}

fn catalog() -> RecordedHeroCatalog {
    RecordedHeroCatalog::new(BTreeMap::from([
        (
            1,
            HeroMetadata {
                name: "Anti-Mage".to_owned(),
                attribute: HeroAttribute::Agility,
                role_weights: BTreeMap::from([
                    ("Carry".to_owned(), 3.0),
                    ("Escape".to_owned(), 3.0),
                    ("Nuker".to_owned(), 1.0),
                ]),
            },
        ),
        (
            2,
            HeroMetadata {
                name: "Axe".to_owned(),
                attribute: HeroAttribute::Strength,
                role_weights: BTreeMap::new(),
            },
        ),
        (
            3,
            HeroMetadata {
                name: "Bane".to_owned(),
                attribute: HeroAttribute::Intelligence,
                role_weights: BTreeMap::new(),
            },
        ),
        (
            5,
            HeroMetadata {
                name: "Crystal Maiden".to_owned(),
                attribute: HeroAttribute::Intelligence,
                role_weights: BTreeMap::from([
                    ("Support".to_owned(), 3.0),
                    ("Disabler".to_owned(), 2.0),
                    ("Nuker".to_owned(), 2.0),
                ]),
            },
        ),
        (
            14,
            HeroMetadata {
                name: "Pudge".to_owned(),
                attribute: HeroAttribute::Strength,
                role_weights: BTreeMap::new(),
            },
        ),
        (
            138,
            HeroMetadata {
                name: "Muerta".to_owned(),
                attribute: HeroAttribute::Universal,
                role_weights: BTreeMap::new(),
            },
        ),
    ]))
}

fn match_row(
    hero_id: i64,
    lane_role: Option<i64>,
    player_slot: u16,
    radiant_win: bool,
) -> ProjectedMatch {
    ProjectedMatch {
        match_id: Some(ValveMatchId(77)),
        hero_id: Some(hero_id),
        lane_role,
        player_slot: Some(player_slot),
        radiant_win,
        kills: 10,
        deaths: 3,
        assists: 8,
        duration_seconds: 2_400,
        start_time: 1_700_000_000,
    }
}

fn profile_request(steam_id: Option<SteamId>) -> ProfileRequest {
    ProfileRequest {
        steam_id,
        ..ProfileRequest::new(USER)
    }
}

#[test]
fn test_get_player_profile_no_steam_id() {
    let service = service(
        FakePlayer::new(None),
        FakeApi::default(),
        FakeClock::new(1_000),
    );
    assert_eq!(
        service
            .get_player_profile(ProfileRequest::new(USER))
            .expect("profile"),
        None
    );
}

#[test]
fn test_get_player_profile_cached() {
    let api = FakeApi::default();
    let service = service(
        FakePlayer::new(Some(STEAM)),
        api.clone(),
        FakeClock::new(1_000),
    );
    let first = service
        .get_player_profile(ProfileRequest::new(USER))
        .expect("first")
        .expect("profile");
    api.set_wins(999);
    let second = service
        .get_player_profile(ProfileRequest::new(USER))
        .expect("cached")
        .expect("profile");
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(second.steam_id, STEAM);
    assert_eq!(second.wins, 100);
    assert_eq!(api.component_calls(), 1);
}

#[test]
fn test_get_player_profile_cache_expired() {
    let api = FakeApi::default();
    let clock = FakeClock::new(1_000);
    let service = service(FakePlayer::new(Some(STEAM)), api.clone(), clock.clone());
    service
        .get_player_profile(ProfileRequest::new(USER))
        .expect("prime cache");
    api.set_wins(150);
    clock.set(1_000 + CACHE_TTL_SECONDS + 100);
    let refreshed = service
        .get_player_profile(ProfileRequest::new(USER))
        .expect("refresh")
        .expect("profile");
    assert_eq!(refreshed.wins, 150);
    assert_eq!(api.component_calls(), 2);
}

#[test]
fn test_get_player_profile_force_refresh() {
    let api = FakeApi::default();
    let service = service(
        FakePlayer::new(Some(STEAM)),
        api.clone(),
        FakeClock::new(1_000),
    );
    service
        .get_player_profile(ProfileRequest::new(USER))
        .expect("prime cache");
    api.set_wins(200);
    let refreshed = service
        .get_player_profile(ProfileRequest {
            force_refresh: true,
            ..ProfileRequest::new(USER)
        })
        .expect("refresh")
        .expect("profile");
    assert_eq!(refreshed.wins, 200);
}

#[test]
fn test_profile_cache_is_not_reused_after_steam_account_changes() {
    let api = FakeApi::default();
    let service = service(FakePlayer::new(None), api.clone(), FakeClock::new(1_000));
    let first = service
        .get_player_profile(profile_request(Some(SteamId(111))))
        .expect("first")
        .expect("profile");
    api.set_wins(20);
    let second = service
        .get_player_profile(profile_request(Some(SteamId(222))))
        .expect("second")
        .expect("profile");
    assert_eq!(first.steam_id, SteamId(111));
    assert_eq!(second.steam_id, SteamId(222));
    assert_eq!(second.wins, 20);
    assert_eq!(api.component_calls(), 2);
}

#[test]
fn test_concurrent_profile_cache_misses_share_one_refresh() {
    let (api, gate) = FakeApi::default().with_identity_gate();
    let service = Arc::new(service(
        FakePlayer::new(None),
        api.clone(),
        FakeClock::new(1_000),
    ));
    let leader_service = Arc::clone(&service);
    let leader = thread::spawn(move || {
        leader_service
            .get_player_profile(profile_request(Some(STEAM)))
            .expect("leader")
            .expect("profile")
    });
    gate.wait_started();
    let follower_service = Arc::clone(&service);
    let follower = thread::spawn(move || {
        follower_service
            .get_player_profile(profile_request(Some(STEAM)))
            .expect("follower")
            .expect("profile")
    });
    thread::sleep(Duration::from_millis(10));
    gate.release();
    let leader = leader.join().expect("leader thread");
    let follower = follower.join().expect("follower thread");
    assert!(Arc::ptr_eq(&leader, &follower));
    assert_eq!(api.component_calls(), 1);
}

#[test]
fn test_profile_components_are_fetched_in_parallel_and_reuse_match_sample() {
    let (api, probe) = FakeApi::default().with_probe(4);
    let service = service(FakePlayer::new(None), api.clone(), FakeClock::new(1_000));
    let recent = vec![match_row(1, Some(1), 0, true)];
    let profile = service
        .get_player_profile(ProfileRequest {
            recent_matches: Some(recent),
            ..profile_request(Some(STEAM))
        })
        .expect("profile")
        .expect("profile data");
    assert_eq!(probe.max_active(), 4);
    assert_eq!(api.projected_calls(), 0);
    assert_eq!(profile.recent_matches[0].match_id, Some(ValveMatchId(77)));
    assert_eq!(profile.last_match_id, Some(ValveMatchId(77)));
}

#[test]
fn test_calc_win_rate() {
    assert_eq!(calculate_win_rate(50, 50), 50.0);
    assert_eq!(calculate_win_rate(75, 25), 75.0);
    assert_eq!(calculate_win_rate(0, 0), 0.0);
    assert_eq!(calculate_win_rate(100, 0), 100.0);
}

#[test]
fn test_did_win_radiant() {
    assert!(did_win(&match_row(1, None, 0, true)));
    assert!(!did_win(&match_row(1, None, 0, false)));
}

#[test]
fn test_did_win_dire() {
    assert!(did_win(&match_row(1, None, 128, false)));
    assert!(!did_win(&match_row(1, None, 128, true)));
}

#[test]
fn test_format_profile_embed_no_profile() {
    let service = service(
        FakePlayer::new(None),
        FakeApi::default(),
        FakeClock::new(1_000),
    );
    assert_eq!(
        service
            .format_profile_embed(USER, "TestUser")
            .expect("embed"),
        None
    );
}

#[test]
fn test_format_profile_embed_success() {
    let service = service(
        FakePlayer::new(Some(STEAM)),
        FakeApi::default(),
        FakeClock::new(1_000),
    );
    let embed = service
        .format_profile_embed(USER, "TestUser")
        .expect("embed")
        .expect("embed plan");
    assert_eq!(embed.title, "Profile: TestUser");
    assert!(embed.fields.len() >= 4);
    assert_eq!(embed.last_match_id, Some(ValveMatchId(77)));
}

#[test]
fn test_calc_attribute_distribution_empty() {
    let service = service(
        FakePlayer::new(None),
        FakeApi::default(),
        FakeClock::new(1_000),
    );
    assert_eq!(
        service.calculate_attribute_distribution(&[]),
        BTreeMap::from([
            ("agi".to_owned(), 0.0),
            ("all".to_owned(), 0.0),
            ("int".to_owned(), 0.0),
            ("str".to_owned(), 0.0),
        ])
    );
}

#[test]
fn test_calc_attribute_distribution_with_data() {
    let service = service(
        FakePlayer::new(None),
        FakeApi::default(),
        FakeClock::new(1_000),
    );
    let matches = [
        match_row(1, None, 0, true),
        match_row(1, None, 0, true),
        match_row(2, None, 0, true),
        match_row(3, None, 0, true),
        match_row(138, None, 0, true),
    ];
    let result = service.calculate_attribute_distribution(&matches);
    assert_eq!(result["agi"], 40.0);
    assert_eq!(result["str"], 20.0);
    assert_eq!(result["int"], 20.0);
    assert_eq!(result["all"], 20.0);
}

#[test]
fn test_calc_lane_distribution_empty() {
    let (result, count) = calculate_lane_distribution(&[]);
    for lane in ["Safe Lane", "Mid", "Off Lane", "Jungle", "Roaming"] {
        assert_eq!(result[lane], 0.0);
    }
    assert_eq!(count, 0);
}

#[test]
fn test_calc_lane_distribution_with_data() {
    let matches = [
        match_row(1, Some(1), 0, true),
        match_row(1, Some(1), 0, true),
        match_row(1, Some(2), 0, true),
        match_row(1, Some(3), 0, true),
        match_row(1, None, 0, true),
    ];
    let (result, count) = calculate_lane_distribution(&matches);
    assert_eq!(result["Safe Lane"], 50.0);
    assert_eq!(result["Mid"], 25.0);
    assert_eq!(result["Off Lane"], 25.0);
    assert_eq!(result["Jungle"], 0.0);
    assert_eq!(result["Roaming"], 0.0);
    assert_eq!(count, 4);
}

#[test]
fn test_calc_lane_distribution_with_roaming() {
    let matches = [
        match_row(1, Some(0), 0, true),
        match_row(1, Some(0), 0, true),
        match_row(1, Some(1), 0, true),
        match_row(1, Some(2), 0, true),
    ];
    let (result, count) = calculate_lane_distribution(&matches);
    assert_eq!(result["Roaming"], 50.0);
    assert_eq!(result["Safe Lane"], 25.0);
    assert_eq!(result["Mid"], 25.0);
    assert_eq!(count, 4);
}

#[test]
fn test_dotabase_role_weights_use_bundled_hero_data() {
    // Recorded from the immutable Dotabase hero-ID catalog. Only the two role
    // rows pinned by the canonical test need full role payloads here; the
    // runtime catalog remains a port rather than a dependency on that package.
    let mut rows = (1..=114)
        .filter(|id| !matches!(id, 1 | 5 | 24))
        .map(|id| RecordedHeroRow {
            id,
            name: "Recorded Hero",
            primary_attribute: "universal",
            roles: "",
            role_levels: "",
        })
        .collect::<Vec<_>>();
    rows.extend([
        RecordedHeroRow {
            id: 1,
            name: "Anti-Mage",
            primary_attribute: "agility",
            roles: "Carry|Escape|Nuker",
            role_levels: "3|3|1",
        },
        RecordedHeroRow {
            id: 5,
            name: "Crystal Maiden",
            primary_attribute: "intelligence",
            roles: "Support|Disabler|Nuker",
            role_levels: "3|2|2",
        },
    ]);
    let catalog = RecordedHeroCatalog::from_rows(&rows)
        .heroes()
        .expect("recorded bundled catalog");
    assert!(catalog.len() > 100);
    assert_eq!(
        catalog[&1].role_weights,
        BTreeMap::from([
            ("Carry".to_owned(), 3.0),
            ("Escape".to_owned(), 3.0),
            ("Nuker".to_owned(), 1.0),
        ])
    );
    assert_eq!(
        catalog[&5].role_weights,
        BTreeMap::from([
            ("Support".to_owned(), 3.0),
            ("Disabler".to_owned(), 2.0),
            ("Nuker".to_owned(), 2.0),
        ])
    );
}

#[test]
fn test_get_full_stats_no_steam_id() {
    let service = service(
        FakePlayer::new(None),
        FakeApi::default(),
        FakeClock::new(1_000),
    );
    assert_eq!(service.get_full_stats(USER, 100).expect("stats"), None);
}

#[test]
fn test_get_full_stats_success() {
    let api = FakeApi::default();
    api.set_wins(500);
    {
        let mut state = api.state.lock().expect("fake API lock");
        state.win_loss.losses = 400;
        state.matches = vec![
            match_row(1, Some(1), 0, true),
            match_row(2, Some(2), 0, false),
        ];
    }
    let service = service(FakePlayer::new(Some(STEAM)), api, FakeClock::new(1_000));
    let result = service
        .get_full_stats(USER, 100)
        .expect("stats")
        .expect("full stats");
    assert_eq!(result.steam_id, STEAM);
    assert_eq!(result.total_wins, 500);
    assert_eq!(result.total_losses, 400);
    assert_eq!(result.attribute_distribution["str"], 50.0);
    assert_eq!(result.lane_distribution["Safe Lane"], 50.0);
    assert_eq!(result.lane_parsed_count, 2);
    assert_eq!(result.top_heroes.len(), 1);
}

#[test]
fn test_get_dota_tab_stats_reuses_matches_for_roles_and_lanes() {
    let player = FakePlayer::new(None);
    let api = FakeApi::default();
    api.set_matches(vec![
        match_row(1, Some(1), 0, true),
        match_row(1, Some(1), 0, false),
        match_row(5, Some(2), 128, false),
    ]);
    let custom_catalog = RecordedHeroCatalog::new(BTreeMap::from([
        (
            1,
            HeroMetadata {
                name: "Anti-Mage".to_owned(),
                attribute: HeroAttribute::Agility,
                role_weights: BTreeMap::from([
                    ("Carry".to_owned(), 3.0),
                    ("Escape".to_owned(), 1.0),
                ]),
            },
        ),
        (
            5,
            HeroMetadata {
                name: "Crystal Maiden".to_owned(),
                attribute: HeroAttribute::Intelligence,
                role_weights: BTreeMap::from([("Support".to_owned(), 2.0)]),
            },
        ),
    ]));
    let service = OpenDotaPlayerService::new(
        player.clone(),
        api.clone(),
        FakeClock::new(1_000),
        custom_catalog,
    );
    let result = service
        .get_dota_tab_stats(USER, 50, Some(STEAM))
        .expect("Dota tab");
    assert_eq!(player.calls(), 0);
    assert_eq!(api.projected_calls(), 1);
    assert_eq!(
        result.role_distribution,
        Some(BTreeMap::from([
            ("Carry".to_owned(), 60.0),
            ("Escape".to_owned(), 20.0),
            ("Support".to_owned(), 20.0),
        ]))
    );
    let full = result.full_stats.as_ref().expect("full stats");
    assert_eq!(full.lane_distribution["Safe Lane"], 66.7);
    assert_eq!(full.lane_distribution["Mid"], 33.3);
    assert_eq!(full.lane_parsed_count, 3);
}

#[test]
fn test_get_dota_tab_stats_overlaps_profile_and_match_requests() {
    let (api, probe) = FakeApi::default().with_probe(5);
    api.set_matches(vec![match_row(1, Some(1), 0, true)]);
    let service = service(FakePlayer::new(None), api, FakeClock::new(1_000));
    let result = service
        .get_dota_tab_stats(USER, 50, Some(STEAM))
        .expect("Dota tab");
    assert_eq!(probe.max_active(), 5);
    assert!(result.role_distribution.is_some());
    assert_eq!(
        result
            .full_stats
            .as_ref()
            .map(|stats| stats.matches_analyzed),
        Some(1)
    );
}

#[test]
fn test_get_dota_tab_stats_caches_complete_result() {
    let api = FakeApi::default();
    api.set_matches(vec![match_row(1, Some(1), 0, true)]);
    let service = service(FakePlayer::new(None), api.clone(), FakeClock::new(1_000));
    let first = service
        .get_dota_tab_stats(USER, 50, Some(STEAM))
        .expect("first");
    let second = service
        .get_dota_tab_stats(USER, 50, Some(STEAM))
        .expect("cached");
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(
        first
            .full_stats
            .as_ref()
            .map(|value| value.matches_analyzed),
        Some(1)
    );
    assert_eq!(api.projected_calls(), 1);
    assert_eq!(api.component_calls(), 1);
}

#[test]
fn test_hero_attributes_use_bundled_data_without_http() {
    let rows = [
        RecordedHeroRow {
            id: 1,
            name: "Anti-Mage",
            primary_attribute: "agility",
            roles: "Carry",
            role_levels: "3",
        },
        RecordedHeroRow {
            id: 2,
            name: "Universal Fixture",
            primary_attribute: "universal",
            roles: "Support",
            role_levels: "1",
        },
    ];
    let api = FakeApi::default();
    let service = OpenDotaPlayerService::new(
        FakePlayer::new(None),
        api.clone(),
        FakeClock::new(1_000),
        RecordedHeroCatalog::from_rows(&rows),
    );
    let result = service.calculate_attribute_distribution(&[
        match_row(1, None, 0, true),
        match_row(2, None, 0, true),
    ]);
    assert_eq!(result["agi"], 50.0);
    assert_eq!(result["all"], 50.0);
    assert_eq!(api.component_calls(), 0);
    assert_eq!(api.projected_calls(), 0);
}

#[test]
fn test_get_dota_tab_stats_preserves_roles_when_profile_is_unavailable() {
    let player = FakePlayer::new(Some(STEAM));
    let api = FakeApi::default();
    api.set_identity(None);
    api.set_matches(vec![
        match_row(5, Some(1), 0, true),
        match_row(5, Some(2), 0, true),
    ]);
    let custom_catalog = RecordedHeroCatalog::new(BTreeMap::from([(
        5,
        HeroMetadata {
            name: "Crystal Maiden".to_owned(),
            attribute: HeroAttribute::Intelligence,
            role_weights: BTreeMap::from([("Support".to_owned(), 3.0), ("Nuker".to_owned(), 1.0)]),
        },
    )]));
    let service = OpenDotaPlayerService::new(
        player.clone(),
        api.clone(),
        FakeClock::new(1_000),
        custom_catalog,
    );
    let result = service
        .get_dota_tab_stats(USER, 50, None)
        .expect("partial Dota tab");
    assert_eq!(player.calls(), 1);
    assert_eq!(api.projected_calls(), 1);
    assert_eq!(
        result.role_distribution,
        Some(BTreeMap::from([
            ("Support".to_owned(), 75.0),
            ("Nuker".to_owned(), 25.0),
        ]))
    );
    assert_eq!(result.full_stats, None);
    let second = service
        .get_dota_tab_stats(USER, 50, Some(STEAM))
        .expect("uncached partial result");
    assert!(!Arc::ptr_eq(&result, &second));
    assert_eq!(api.projected_calls(), 2);
}
