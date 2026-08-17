use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use super::*;

const GUILD: GuildId = GuildId(86_753_090);
const MATCH_TIME: i64 = 1_705_320_000;

type ParticipantRows = BTreeMap<(GuildId, InternalMatchId), Vec<InternalParticipant>>;
type SteamIdRows = BTreeMap<(GuildId, UserId), Vec<SteamId>>;
type SteamIdCalls = Vec<(Vec<UserId>, GuildId)>;
type OpenSkillCalls = Vec<(InternalMatchId, Option<GuildId>)>;
type SteamIdBulkLinkCalls = Vec<Vec<(UserId, SteamId)>>;

#[derive(Clone, Default)]
struct FakeMatchRepository {
    matches: Arc<Mutex<BTreeMap<(GuildId, InternalMatchId), InternalMatch>>>,
    participants: Arc<Mutex<ParticipantRows>>,
    point_match_calls: Arc<AtomicUsize>,
    point_participant_calls: Arc<AtomicUsize>,
    bulk_participant_calls: Arc<AtomicUsize>,
}

impl FakeMatchRepository {
    fn insert(
        &self,
        guild_id: GuildId,
        internal_match: InternalMatch,
        participants: Vec<InternalParticipant>,
    ) {
        let match_id = internal_match.match_id;
        self.matches
            .lock()
            .expect("matches lock")
            .insert((guild_id, match_id), internal_match);
        self.participants
            .lock()
            .expect("participants lock")
            .insert((guild_id, match_id), participants);
    }
}

impl MatchDiscoveryReadPort for FakeMatchRepository {
    fn get_match(
        &self,
        match_id: InternalMatchId,
        guild_id: GuildId,
    ) -> Result<Option<InternalMatch>, DiscoveryPortError> {
        self.point_match_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .matches
            .lock()
            .expect("matches lock")
            .get(&(guild_id, match_id))
            .cloned())
    }

    fn get_match_participants(
        &self,
        match_id: InternalMatchId,
        guild_id: GuildId,
    ) -> Result<Vec<InternalParticipant>, DiscoveryPortError> {
        self.point_participant_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .participants
            .lock()
            .expect("participants lock")
            .get(&(guild_id, match_id))
            .cloned()
            .unwrap_or_default())
    }

    fn get_matches_without_enrichment(
        &self,
        guild_id: GuildId,
        _limit: usize,
    ) -> Result<Vec<InternalMatch>, DiscoveryPortError> {
        Ok(self
            .matches
            .lock()
            .expect("matches lock")
            .iter()
            .filter(|((row_guild, _), _)| *row_guild == guild_id)
            .map(|(_, internal_match)| internal_match.clone())
            .collect())
    }

    fn get_match_participants_bulk(
        &self,
        match_ids: &[InternalMatchId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<InternalMatchId, Vec<InternalParticipant>>, DiscoveryPortError> {
        self.bulk_participant_calls.fetch_add(1, Ordering::SeqCst);
        let participants = self.participants.lock().expect("participants lock");
        Ok(match_ids
            .iter()
            .map(|match_id| {
                (
                    *match_id,
                    participants
                        .get(&(guild_id, *match_id))
                        .cloned()
                        .unwrap_or_default(),
                )
            })
            .collect())
    }
}

#[derive(Clone, Default)]
struct FakePlayerRepository {
    steam_ids: Arc<Mutex<SteamIdRows>>,
    calls: Arc<Mutex<SteamIdCalls>>,
}

impl FakePlayerRepository {
    fn link(&self, guild_id: GuildId, discord_id: UserId, steam_ids: Vec<SteamId>) {
        self.steam_ids
            .lock()
            .expect("steam lock")
            .insert((guild_id, discord_id), steam_ids);
    }
}

impl PlayerSteamIdPort for FakePlayerRepository {
    fn get_steam_ids_bulk(
        &self,
        discord_ids: &[UserId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<UserId, Vec<SteamId>>, DiscoveryPortError> {
        self.calls
            .lock()
            .expect("calls lock")
            .push((discord_ids.to_vec(), guild_id));
        let steam_ids = self.steam_ids.lock().expect("steam lock");
        Ok(discord_ids
            .iter()
            .map(|discord_id| {
                (
                    *discord_id,
                    steam_ids
                        .get(&(guild_id, *discord_id))
                        .cloned()
                        .unwrap_or_default(),
                )
            })
            .collect())
    }
}

type HistoryResponse = Result<Option<Vec<PlayerHistoryMatch>>, DiscoveryPortError>;
type DetailResponse = Result<Option<OpenDotaMatchDetails>, DiscoveryPortError>;

#[derive(Default)]
struct ApiGate {
    blocked: bool,
    entered: bool,
}

#[derive(Clone, Default)]
struct FakeOpenDota {
    histories: Arc<Mutex<BTreeMap<SteamId, VecDeque<HistoryResponse>>>>,
    default_history: Arc<Mutex<Option<HistoryResponse>>>,
    details: Arc<Mutex<BTreeMap<ValveMatchId, VecDeque<DetailResponse>>>>,
    history_calls: Arc<Mutex<Vec<SteamId>>>,
    detail_calls: Arc<Mutex<Vec<ValveMatchId>>>,
    gate: Arc<(Mutex<ApiGate>, Condvar)>,
}

impl FakeOpenDota {
    fn set_history(&self, steam_id: SteamId, responses: Vec<HistoryResponse>) {
        self.histories
            .lock()
            .expect("histories lock")
            .insert(steam_id, responses.into());
    }

    fn set_default_history(&self, response: HistoryResponse) {
        *self.default_history.lock().expect("default history lock") = Some(response);
    }

    fn set_details(&self, match_id: ValveMatchId, responses: Vec<DetailResponse>) {
        self.details
            .lock()
            .expect("details lock")
            .insert(match_id, responses.into());
    }

    fn history_call_count(&self) -> usize {
        self.history_calls.lock().expect("history calls lock").len()
    }

    fn detail_call_count(&self) -> usize {
        self.detail_calls.lock().expect("detail calls lock").len()
    }

    fn block_histories(&self) {
        let (gate, _) = &*self.gate;
        gate.lock().expect("gate lock").blocked = true;
    }

    fn wait_until_history_entered(&self) {
        let (gate, condition) = &*self.gate;
        let mut state = gate.lock().expect("gate lock");
        while !state.entered {
            state = condition.wait(state).expect("gate wait");
        }
    }

    fn release_histories(&self) {
        let (gate, condition) = &*self.gate;
        let mut state = gate.lock().expect("gate lock");
        state.blocked = false;
        condition.notify_all();
    }
}

impl OpenDotaDiscoveryPort for FakeOpenDota {
    fn player_matches(
        &self,
        steam_id: SteamId,
        limit: usize,
    ) -> Result<Option<Vec<PlayerHistoryMatch>>, DiscoveryPortError> {
        assert_eq!(limit, 100);
        self.history_calls
            .lock()
            .expect("history calls lock")
            .push(steam_id);
        let (gate, condition) = &*self.gate;
        let mut gate_state = gate.lock().expect("gate lock");
        if gate_state.blocked {
            gate_state.entered = true;
            condition.notify_all();
            while gate_state.blocked {
                gate_state = condition.wait(gate_state).expect("gate wait");
            }
        }
        drop(gate_state);

        if let Some(queue) = self
            .histories
            .lock()
            .expect("histories lock")
            .get_mut(&steam_id)
            && let Some(response) = queue.pop_front()
        {
            return response;
        }
        self.default_history
            .lock()
            .expect("default history lock")
            .clone()
            .unwrap_or(Ok(Some(Vec::new())))
    }

    fn match_details(
        &self,
        match_id: ValveMatchId,
    ) -> Result<Option<OpenDotaMatchDetails>, DiscoveryPortError> {
        self.detail_calls
            .lock()
            .expect("detail calls lock")
            .push(match_id);
        self.details
            .lock()
            .expect("details lock")
            .get_mut(&match_id)
            .and_then(VecDeque::pop_front)
            .unwrap_or(Ok(None))
    }
}

#[derive(Clone)]
struct FakeEnrichment {
    requests: Arc<Mutex<Vec<DiscoveryEnrichmentRequest>>>,
    outcome: Arc<Mutex<DiscoveryEnrichmentOutcome>>,
}

impl Default for FakeEnrichment {
    fn default() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            outcome: Arc::new(Mutex::new(DiscoveryEnrichmentOutcome {
                success: true,
                validation_error: None,
            })),
        }
    }
}

impl DiscoveryEnrichmentPort for FakeEnrichment {
    fn enrich_match(
        &self,
        request: DiscoveryEnrichmentRequest,
    ) -> Result<DiscoveryEnrichmentOutcome, DiscoveryPortError> {
        self.requests
            .lock()
            .expect("enrichment requests lock")
            .push(request);
        Ok(self.outcome.lock().expect("outcome lock").clone())
    }
}

#[derive(Clone, Default)]
struct FakeClock {
    now: Arc<AtomicI64>,
    sleeps: Arc<Mutex<Vec<Duration>>>,
}

impl DiscoveryClock for FakeClock {
    fn now_epoch_seconds(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }

    fn sleep(&self, duration: Duration) {
        self.sleeps.lock().expect("sleeps lock").push(duration);
    }
}

type TestService = MatchDiscoveryService<
    FakeMatchRepository,
    FakePlayerRepository,
    FakeOpenDota,
    FakeEnrichment,
    FakeClock,
>;

fn participants() -> Vec<InternalParticipant> {
    (1..=10)
        .map(|discord_id| InternalParticipant {
            discord_id: UserId(discord_id),
            side: if discord_id <= 5 {
                TeamSide::Radiant
            } else {
                TeamSide::Dire
            },
        })
        .collect()
}

fn internal_match(match_id: i64, timestamp: i64) -> InternalMatch {
    InternalMatch {
        match_id: InternalMatchId(match_id),
        match_date: MatchTimeValue::Unix(timestamp as f64),
        winning_team: TeamSide::Radiant,
    }
}

fn setup_single() -> (
    TestService,
    FakeMatchRepository,
    FakePlayerRepository,
    FakeOpenDota,
    FakeEnrichment,
    FakeClock,
) {
    let matches = FakeMatchRepository::default();
    matches.insert(GUILD, internal_match(1, MATCH_TIME), participants());
    let players = FakePlayerRepository::default();
    for discord_id in 1..=10 {
        players.link(GUILD, UserId(discord_id), vec![SteamId(discord_id + 1_000)]);
    }
    let api = FakeOpenDota::default();
    let enrichment = FakeEnrichment::default();
    let clock = FakeClock::default();
    clock.now.store(MATCH_TIME + 1_000, Ordering::SeqCst);
    let service = MatchDiscoveryService::new(
        matches.clone(),
        players.clone(),
        api.clone(),
        enrichment.clone(),
        clock.clone(),
    );
    (service, matches, players, api, enrichment, clock)
}

fn history(match_id: i64, timestamp: i64) -> Vec<PlayerHistoryMatch> {
    vec![PlayerHistoryMatch {
        match_id: ValveMatchId(match_id),
        start_time: timestamp,
    }]
}

fn shared_batch() -> (
    TestService,
    FakeMatchRepository,
    FakePlayerRepository,
    FakeOpenDota,
    FakeEnrichment,
    FakeClock,
) {
    let (service, matches, players, api, enrichment, clock) = setup_single();
    matches.insert(
        GUILD,
        internal_match(2, MATCH_TIME + 86_400),
        participants(),
    );
    api.set_default_history(Ok(Some(vec![
        PlayerHistoryMatch {
            match_id: ValveMatchId(9_001),
            start_time: MATCH_TIME,
        },
        PlayerHistoryMatch {
            match_id: ValveMatchId(9_002),
            start_time: MATCH_TIME + 86_400,
        },
    ])));
    (service, matches, players, api, enrichment, clock)
}

#[test]
fn test_discover_match_high_confidence() {
    let (service, _, _, api, _, _) = setup_single();
    api.set_default_history(Ok(Some(history(99_999, MATCH_TIME + 60))));

    let result = service.discover_match(InternalMatchId(1), Some(GUILD), true);

    assert_eq!(result.status, DiscoveryStatus::Discovered);
    assert_eq!(result.valve_match_id, Some(ValveMatchId(99_999)));
    assert_eq!(result.confidence, Some(1.0));
    assert_eq!(result.player_count, 10);
}

#[test]
fn test_discovery_stops_after_anchor_when_details_validate_roster() {
    let (service, _, _, api, _, _) = setup_single();
    api.set_default_history(Ok(Some(history(99_999, MATCH_TIME))));
    api.set_details(
        ValveMatchId(99_999),
        vec![Ok(Some(OpenDotaMatchDetails::roster(
            ValveMatchId(99_999),
            (1..=10).map(|id| SteamId(id + 1_000)),
        )))],
    );

    let result = service.discover_match(InternalMatchId(1), Some(GUILD), true);

    assert_eq!(result.status, DiscoveryStatus::Discovered);
    assert_eq!(result.player_count, 10);
    assert_eq!(api.history_call_count(), 1);
    assert_eq!(api.detail_call_count(), 1);
}

#[test]
fn test_discovery_reuses_validated_details_during_enrichment() {
    let (service, _, _, api, enrichment, _) = setup_single();
    api.set_default_history(Ok(Some(history(99_999, MATCH_TIME))));
    let details =
        OpenDotaMatchDetails::roster(ValveMatchId(99_999), (1..=10).map(|id| SteamId(id + 1_000)));
    api.set_details(ValveMatchId(99_999), vec![Ok(Some(details.clone()))]);

    let result = service.discover_match(InternalMatchId(1), Some(GUILD), false);

    assert_eq!(result.status, DiscoveryStatus::Discovered);
    assert_eq!(api.detail_call_count(), 1);
    let requests = enrichment.requests.lock().expect("requests lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].source, EnrichmentSource::Auto);
    assert_eq!(requests[0].confidence, Some(1.0));
    assert_eq!(requests[0].validated_details.as_ref(), Some(&details));
}

#[test]
fn test_validated_candidate_not_replaced_by_unvalidated_naive_count() {
    let (service, _, _, api, _, _) = setup_single();
    for id in 1..=5 {
        api.set_history(
            SteamId(id + 1_000),
            vec![Ok(Some(history(99_999, MATCH_TIME + 10)))],
        );
    }
    api.set_history(
        SteamId(1_006),
        vec![Ok(Some(history(88_888, MATCH_TIME + 60)))],
    );
    api.set_details(
        ValveMatchId(88_888),
        vec![Ok(Some(OpenDotaMatchDetails::roster(
            ValveMatchId(88_888),
            [SteamId(1_001), SteamId(1_006)],
        )))],
    );
    let config = DiscoveryConfig {
        minimum_roster_matches: 2,
        ..DiscoveryConfig::default()
    };
    let service = service.with_config(config);

    let result = service.discover_match(InternalMatchId(1), Some(GUILD), true);

    assert_eq!(result.status, DiscoveryStatus::Discovered);
    assert_eq!(result.valve_match_id, Some(ValveMatchId(88_888)));
    assert_eq!(result.player_count, 2);
}

#[test]
fn test_discover_match_low_confidence_skipped() {
    let (service, _, _, api, _, _) = setup_single();
    for id in 1..=5 {
        api.set_history(
            SteamId(id + 1_000),
            vec![Ok(Some(history(99_999, MATCH_TIME)))],
        );
    }

    let result = service.discover_match(InternalMatchId(1), Some(GUILD), true);

    assert_eq!(result.status, DiscoveryStatus::LowConfidence);
    assert_eq!(result.confidence, Some(0.5));
    assert_eq!(result.player_count, 5);
}

#[test]
fn test_discover_match_not_enough_steam_ids() {
    let (service, _, players, api, _, _) = setup_single();
    for id in 4..=10 {
        players.link(GUILD, UserId(id), Vec::new());
    }

    let result = service.discover_match(InternalMatchId(1), Some(GUILD), true);

    assert_eq!(result.status, DiscoveryStatus::NoSteamIds);
    assert_eq!(result.players_with_steam_id, 3);
    assert_eq!(api.history_call_count(), 0);
}

#[test]
fn test_discover_match_outside_time_window() {
    let (service, _, _, api, _, _) = setup_single();
    api.set_default_history(Ok(Some(history(99_999, MATCH_TIME + 5 * 3_600))));

    let result = service.discover_match(InternalMatchId(1), Some(GUILD), true);

    assert_eq!(result.status, DiscoveryStatus::NoCandidates);
}

#[test]
fn test_discover_match_dry_run_no_enrichment() {
    let (service, _, _, api, enrichment, _) = setup_single();
    api.set_default_history(Ok(Some(history(99_999, MATCH_TIME))));

    let result = service.discover_match(InternalMatchId(1), Some(GUILD), true);

    assert_eq!(result.status, DiscoveryStatus::Discovered);
    assert!(
        enrichment
            .requests
            .lock()
            .expect("requests lock")
            .is_empty()
    );
}

#[test]
fn test_discover_all_matches() {
    let (service, matches, players, _, _, _) = setup_single();
    matches.insert(
        GUILD,
        internal_match(2, MATCH_TIME + 86_400),
        participants(),
    );
    for id in 4..=10 {
        players.link(GUILD, UserId(id), Vec::new());
    }

    let result = service.discover_all_matches(Some(GUILD), true);

    assert_eq!(result.total_unenriched, 2);
    assert_eq!(result.skipped_no_steam_ids, 2);
    assert_eq!(result.discovered, 0);
}

#[test]
fn test_discover_all_matches_reuses_shared_player_histories() {
    let (service, matches, players, api, _, _) = shared_batch();

    let result = service.discover_all_matches(Some(GUILD), true);

    assert_eq!(result.discovered, 2);
    assert_eq!(api.history_call_count(), 10);
    assert_eq!(matches.bulk_participant_calls.load(Ordering::SeqCst), 1);
    assert_eq!(matches.point_match_calls.load(Ordering::SeqCst), 0);
    assert_eq!(matches.point_participant_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        players.calls.lock().expect("player calls").as_slice(),
        &[((1..=10).map(UserId).collect(), GUILD)]
    );
}

#[test]
fn test_discover_all_matches_does_not_throttle_without_history_requests() {
    let (service, _, players, api, _, clock) = setup_single();
    for id in 1..=10 {
        players.link(GUILD, UserId(id), Vec::new());
    }

    let result = service.discover_all_matches(Some(GUILD), true);

    assert_eq!(result.skipped_no_steam_ids, 1);
    assert_eq!(api.history_call_count(), 0);
    assert!(clock.sleeps.lock().expect("sleeps lock").is_empty());
}

fn assert_transient_history_retry(first: HistoryResponse) {
    let (service, _, _, api, _, clock) = shared_batch();
    let histories = vec![
        PlayerHistoryMatch {
            match_id: ValveMatchId(9_001),
            start_time: MATCH_TIME,
        },
        PlayerHistoryMatch {
            match_id: ValveMatchId(9_002),
            start_time: MATCH_TIME + 86_400,
        },
    ];
    api.set_history(SteamId(1_001), vec![first, Ok(Some(histories.clone()))]);

    let result = service.discover_all_matches(Some(GUILD), true);

    assert_eq!(result.skipped_low_confidence, 1);
    assert_eq!(result.discovered, 1);
    assert_eq!(api.history_call_count(), 11);
    assert_eq!(clock.sleeps.lock().expect("sleeps lock").len(), 1);
}

#[test]
fn test_discover_all_matches_retries_transient_history_failures_none() {
    assert_transient_history_retry(Ok(None));
}

#[test]
fn test_discover_all_matches_retries_transient_history_failures_failure1() {
    assert_transient_history_retry(Err(DiscoveryPortError::new("temporary failure")));
}

#[test]
fn test_discover_all_matches_caches_successful_empty_histories() {
    let (service, _, _, api, _, clock) = shared_batch();
    api.set_history(SteamId(1_001), vec![Ok(Some(Vec::new()))]);

    let result = service.discover_all_matches(Some(GUILD), true);

    assert_eq!(result.skipped_low_confidence, 2);
    assert_eq!(api.history_call_count(), 10);
    assert!(clock.sleeps.lock().expect("sleeps lock").is_empty());
}

#[test]
fn test_parse_match_time_iso_format() {
    assert!(parse_match_time(&MatchTimeValue::Text("2024-01-15T12:00:00Z".to_owned())).is_some());
    assert!(
        parse_match_time(&MatchTimeValue::Text(
            "2024-01-15T12:00:00+00:00".to_owned()
        ))
        .is_some()
    );
}

#[test]
fn test_parse_match_time_sqlite_format() {
    assert!(parse_match_time(&MatchTimeValue::Text("2024-01-15 12:00:00".to_owned())).is_some());
}

#[test]
fn test_parse_match_time_unix_timestamp() {
    assert_eq!(
        parse_match_time(&MatchTimeValue::Unix(1_705_320_000.0)),
        Some(1_705_320_000)
    );
}

#[test]
fn test_parse_match_time_invalid() {
    assert_eq!(parse_match_time(&MatchTimeValue::Missing), None);
    assert_eq!(
        parse_match_time(&MatchTimeValue::Text("invalid".to_owned())),
        None
    );
}

#[test]
fn test_parse_match_time_sqlite_utc_correct() {
    assert_eq!(
        parse_match_time(&MatchTimeValue::Text("2024-01-15 12:00:00".to_owned())),
        Some(1_705_320_000)
    );
}

#[derive(Clone, Default)]
struct RecordingWriter {
    writes: Arc<Mutex<Vec<EnrichmentWrite>>>,
    farm_stats: Arc<Mutex<Vec<PositionFarmRow>>>,
    stored_estimates: Arc<Mutex<Vec<(i64, String)>>>,
}

impl EnrichmentWritePort for RecordingWriter {
    fn apply_enrichment_atomic(&self, write: EnrichmentWrite) -> Result<(), DiscoveryPortError> {
        self.writes.lock().expect("writes lock").push(write);
        Ok(())
    }

    fn get_participant_farm_stats(
        &self,
        _match_id: InternalMatchId,
        _guild_id: GuildId,
    ) -> Result<Vec<PositionFarmRow>, DiscoveryPortError> {
        Ok(self.farm_stats.lock().expect("farm stats lock").clone())
    }

    fn store_estimated_positions(
        &self,
        _match_id: InternalMatchId,
        _guild_id: GuildId,
        estimates: &[(i64, &str)],
    ) -> Result<usize, DiscoveryPortError> {
        let mut stored = self.stored_estimates.lock().expect("stored estimates lock");
        stored.extend(
            estimates
                .iter()
                .map(|(discord_id, position)| (*discord_id, (*position).to_owned())),
        );
        Ok(estimates.len())
    }
}

#[derive(Clone, Default)]
struct FakeOpenSkill {
    calls: Arc<Mutex<OpenSkillCalls>>,
}

impl OpenSkillEnrichmentPort for FakeOpenSkill {
    fn update_openskill_ratings_for_match(
        &self,
        match_id: InternalMatchId,
        guild_id: Option<GuildId>,
    ) -> Result<OpenSkillEnrichmentResult, DiscoveryPortError> {
        self.calls
            .lock()
            .expect("openskill calls lock")
            .push((match_id, guild_id));
        Ok(OpenSkillEnrichmentResult {
            success: true,
            players_updated: 1,
            error: None,
        })
    }
}

#[derive(Clone, Default)]
struct FakeBackfillRepository {
    candidates: Arc<Mutex<Vec<SteamIdBackfillCandidate>>>,
    outcomes: Arc<Mutex<Vec<BulkSteamIdLinkOutcome>>>,
    calls: Arc<Mutex<SteamIdBulkLinkCalls>>,
}

impl SteamIdBackfillPort for FakeBackfillRepository {
    fn players_missing_steam_ids(
        &self,
    ) -> Result<Vec<SteamIdBackfillCandidate>, DiscoveryPortError> {
        Ok(self.candidates.lock().expect("candidates lock").clone())
    }

    fn add_steam_ids_bulk(
        &self,
        links: &[(UserId, SteamId)],
    ) -> Result<Vec<BulkSteamIdLinkOutcome>, DiscoveryPortError> {
        self.calls
            .lock()
            .expect("bulk link calls lock")
            .push(links.to_vec());
        Ok(self.outcomes.lock().expect("outcomes lock").clone())
    }
}

#[derive(Clone, Default)]
struct FakeDotabuffExtractor {
    responses: Arc<Mutex<VecDeque<Option<SteamId>>>>,
}

impl DotabuffIdExtractionPort for FakeDotabuffExtractor {
    fn extract_player_id_from_dotabuff(&self, _url: &str) -> Option<SteamId> {
        self.responses
            .lock()
            .expect("dotabuff responses lock")
            .pop_front()
            .flatten()
    }
}

fn enrichment_player(account_id: i64) -> OpenDotaPlayer {
    OpenDotaPlayer {
        account_id: Some(SteamId(account_id)),
        player_slot: Some(0),
        stats: EnrichedParticipantStats {
            hero_id: 1,
            kills: 10,
            deaths: 2,
            assists: 5,
            gpm: 600,
            xpm: 550,
            hero_damage: 25_000,
            tower_damage: 3_000,
            last_hits: 200,
            denies: 10,
            net_worth: 20_000,
            hero_healing: 0,
            lane_role: Some(2),
            lane_efficiency: None,
            ..EnrichedParticipantStats::default()
        },
        fantasy: FantasyStats {
            kills: 10.0,
            deaths: Some(2.0),
            assists: 5.0,
            gold_per_min: 600.0,
            xp_per_min: 550.0,
            last_hits: 200.0,
            denies: 10.0,
            ..FantasyStats::default()
        },
        wrapped: WrappedPlayerTelemetry::default(),
    }
}

fn enrichment_details(players: Vec<OpenDotaPlayer>) -> OpenDotaMatchDetails {
    OpenDotaMatchDetails {
        match_id: ValveMatchId(8_181_518_332),
        duration_seconds: 2_400,
        radiant_win: true,
        radiant_score: 35,
        dire_score: 22,
        game_mode: 2,
        comeback: None,
        throw_amount: None,
        raw_payload: Some("{\"match_id\":8181518332}".to_owned()),
        players,
    }
}

fn service_request() -> EnrichMatchRequest {
    let mut request =
        EnrichMatchRequest::manual(InternalMatchId(1), ValveMatchId(8_181_518_332), Some(GUILD));
    request.skip_validation = true;
    request
}

fn one_player_enrichment_fixture() -> (
    FakeMatchRepository,
    FakePlayerRepository,
    FakeOpenDota,
    RecordingWriter,
    FakeOpenSkill,
) {
    let matches = FakeMatchRepository::default();
    matches.insert(
        GUILD,
        internal_match(1, MATCH_TIME),
        vec![InternalParticipant {
            discord_id: UserId(100),
            side: TeamSide::Radiant,
        }],
    );
    let players = FakePlayerRepository::default();
    players.link(GUILD, UserId(100), vec![SteamId(12_345)]);
    (
        matches,
        players,
        FakeOpenDota::default(),
        RecordingWriter::default(),
        FakeOpenSkill::default(),
    )
}

fn one_player_input(source: EnrichmentSource, confidence: Option<f64>) -> EnrichmentInput {
    EnrichmentInput {
        internal_match: internal_match(1, MATCH_TIME),
        valve_match_id: ValveMatchId(99_999),
        guild_id: GUILD,
        details: OpenDotaMatchDetails {
            match_id: ValveMatchId(99_999),
            duration_seconds: 2_400,
            radiant_win: true,
            radiant_score: 35,
            dire_score: 22,
            game_mode: 2,
            players: vec![OpenDotaPlayer {
                account_id: Some(SteamId(42)),
                player_slot: Some(0),
                fantasy: FantasyStats {
                    kills: 3.0,
                    deaths: Some(2.0),
                    assists: 5.0,
                    ..FantasyStats::default()
                },
                ..OpenDotaPlayer::default()
            }],
            comeback: None,
            throw_amount: None,
            raw_payload: None,
        },
        participants: vec![InternalParticipant {
            discord_id: UserId(111),
            side: TeamSide::Radiant,
        }],
        discord_to_steam_ids: BTreeMap::from([(UserId(111), vec![SteamId(42)])]),
        source,
        confidence,
        skip_validation: true,
        minimum_roster_matches: 10,
    }
}

#[test]
fn test_enrich_match_default_source_manual() {
    let writer = RecordingWriter::default();
    let policy = MatchEnrichmentPolicy::new(writer.clone());

    let result = policy.enrich_match(one_player_input(EnrichmentSource::default(), None));

    assert!(result.success);
    let writes = writer.writes.lock().expect("writes lock");
    assert_eq!(writes[0].source, EnrichmentSource::Manual);
    assert_eq!(writes[0].confidence, None);
}

#[test]
fn test_enrich_match_auto_source() {
    let writer = RecordingWriter::default();
    let policy = MatchEnrichmentPolicy::new(writer.clone());

    let result = policy.enrich_match(one_player_input(EnrichmentSource::Auto, Some(0.9)));

    assert!(result.success);
    let writes = writer.writes.lock().expect("writes lock");
    assert_eq!(writes[0].source, EnrichmentSource::Auto);
    assert_eq!(writes[0].confidence, Some(0.9));
}

#[test]
fn test_calculate_fantasy_points_basic() {
    let stats = FantasyStats {
        kills: 10.0,
        deaths: Some(5.0),
        assists: 15.0,
        last_hits: 200.0,
        denies: 10.0,
        gold_per_min: 500.0,
        xp_per_min: 450.0,
        ..FantasyStats::default()
    };

    assert_eq!(calculate_fantasy_points(&stats), 9.28);
}

#[test]
fn test_calculate_fantasy_points_with_objectives() {
    let stats = FantasyStats {
        kills: 5.0,
        deaths: Some(2.0),
        assists: 10.0,
        last_hits: 100.0,
        denies: 5.0,
        gold_per_min: 400.0,
        xp_per_min: 350.0,
        towers_killed: 3.0,
        roshans_killed: 1.0,
        ..FantasyStats::default()
    };

    assert_eq!(calculate_fantasy_points(&stats), 11.21);
}

#[test]
fn test_calculate_fantasy_points_support_style() {
    let stats = FantasyStats {
        kills: 2.0,
        deaths: Some(8.0),
        assists: 25.0,
        last_hits: 50.0,
        gold_per_min: 250.0,
        xp_per_min: 300.0,
        obs_placed: 15.0,
        sen_placed: 10.0,
        camps_stacked: 8.0,
        teamfight_participation: 0.85,
        hero_healing: 5_000.0,
        ..FantasyStats::default()
    };

    assert_eq!(calculate_fantasy_points(&stats), 27.25);
}

#[test]
fn test_calculate_fantasy_points_first_blood() {
    let stats = FantasyStats {
        kills: 1.0,
        deaths: Some(0.0),
        firstblood_claimed: true,
        ..FantasyStats::default()
    };

    assert_eq!(calculate_fantasy_points(&stats), 7.3);
}

#[test]
fn test_calculate_fantasy_points_stuns() {
    let stats = FantasyStats {
        deaths: Some(0.0),
        stuns: 100.0,
        ..FantasyStats::default()
    };

    assert_eq!(calculate_fantasy_points(&stats), 10.0);
}

#[test]
fn test_calculate_fantasy_points_offlane_style() {
    let stats = FantasyStats {
        kills: 4.0,
        deaths: Some(6.0),
        assists: 18.0,
        last_hits: 150.0,
        denies: 8.0,
        gold_per_min: 380.0,
        xp_per_min: 550.0,
        towers_killed: 2.0,
        stuns: 85.0,
        teamfight_participation: 0.75,
        ..FantasyStats::default()
    };

    assert_eq!(calculate_fantasy_points(&stats), 17.63);
}

#[test]
fn test_calculate_fantasy_points_empty() {
    assert_eq!(calculate_fantasy_points(&FantasyStats::default()), 0.0);
}

#[test]
fn test_validation_winning_team_mismatch() {
    let writer = RecordingWriter::default();
    let policy = MatchEnrichmentPolicy::new(writer.clone());
    let mut input = one_player_input(EnrichmentSource::Manual, None);
    input.participants.clear();
    input.discord_to_steam_ids.clear();
    input.details.players.clear();
    input.details.radiant_win = false;
    input.skip_validation = false;
    input.minimum_roster_matches = 0;

    let result = policy.enrich_match(input);

    assert!(!result.success);
    assert!(matches!(
        result.validation_error,
        Some(EnrichmentValidationError::WinningTeamMismatch { .. })
    ));
    assert!(writer.writes.lock().expect("writes lock").is_empty());
}

#[test]
fn test_validation_player_side_mismatch() {
    let writer = RecordingWriter::default();
    let policy = MatchEnrichmentPolicy::new(writer.clone());
    let mut input = one_player_input(EnrichmentSource::Manual, None);
    input.details.players[0].player_slot = Some(128);
    input.skip_validation = false;
    input.minimum_roster_matches = 1;

    let result = policy.enrich_match(input);

    assert!(!result.success);
    assert!(matches!(
        result.validation_error,
        Some(EnrichmentValidationError::PlayerSideMismatch { .. })
    ));
    assert!(writer.writes.lock().expect("writes lock").is_empty());
}

#[test]
fn test_validation_success_with_full_match() {
    let writer = RecordingWriter::default();
    let policy = MatchEnrichmentPolicy::new(writer.clone());
    let participants = participants();
    let steam_ids = (1..=10)
        .map(|id| (UserId(id), vec![SteamId(id + 1_000)]))
        .collect::<BTreeMap<_, _>>();
    let players = participants
        .iter()
        .map(|participant| OpenDotaPlayer {
            account_id: Some(SteamId(participant.discord_id.0 + 1_000)),
            player_slot: Some(match participant.side {
                TeamSide::Radiant => (participant.discord_id.0 - 1) as u16,
                TeamSide::Dire => (128 + participant.discord_id.0 - 6) as u16,
            }),
            fantasy: FantasyStats {
                kills: 5.0,
                deaths: Some(2.0),
                assists: 10.0,
                last_hits: 100.0,
                gold_per_min: 400.0,
                ..FantasyStats::default()
            },
            ..OpenDotaPlayer::default()
        })
        .collect();
    let input = EnrichmentInput {
        internal_match: internal_match(1, MATCH_TIME),
        valve_match_id: ValveMatchId(99_999),
        guild_id: GUILD,
        details: OpenDotaMatchDetails {
            match_id: ValveMatchId(99_999),
            duration_seconds: 2_400,
            radiant_win: true,
            radiant_score: 35,
            dire_score: 22,
            game_mode: 2,
            comeback: None,
            throw_amount: None,
            raw_payload: None,
            players,
        },
        participants,
        discord_to_steam_ids: steam_ids,
        source: EnrichmentSource::Manual,
        confidence: None,
        skip_validation: false,
        minimum_roster_matches: 10,
    };

    let result = policy.enrich_match(input);

    assert!(result.success);
    assert_eq!(result.players_enriched, 10);
    assert!(result.radiant_fantasy > 0.0);
    assert!(result.dire_fantasy > 0.0);
    assert_eq!(writer.writes.lock().expect("writes lock").len(), 1);
}

#[test]
fn test_enrich_match_success() {
    let (matches, players, api, writer, _) = one_player_enrichment_fixture();
    api.set_details(
        ValveMatchId(8_181_518_332),
        vec![Ok(Some(enrichment_details(vec![enrichment_player(
            12_345,
        )])))],
    );
    let service =
        MatchEnrichmentService::new(matches, players, api, writer.clone(), None::<FakeOpenSkill>);

    let result = service.enrich_match(service_request());

    assert!(result.success);
    assert_eq!(result.players_enriched, 1);
    assert_eq!(result.duration_seconds, 2_400);
    assert!(result.radiant_win);
    assert!(result.fantasy_points_calculated);
    let writes = writer.writes.lock().expect("writes lock");
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].participant_updates[0].stats.hero_id, 1);
    assert_eq!(writes[0].participant_updates[0].stats.kills, 10);
}

#[test]
fn test_enrich_match_estimates_and_stores_positions_on_success() {
    let (matches, players, api, writer, _) = one_player_enrichment_fixture();
    api.set_details(
        ValveMatchId(8_181_518_332),
        vec![Ok(Some(enrichment_details(vec![enrichment_player(
            12_345,
        )])))],
    );
    let farm_member =
        |discord_id: i64, lane_role: i64, last_hits: i64, wards: i64| PositionFarmRow {
            team_number: if discord_id <= 5 { 1 } else { 2 },
            stats: ParticipantFarmStats {
                discord_id,
                lane_role: Some(lane_role),
                last_hits: Some(last_hits),
                net_worth: None,
                gpm: None,
                obs_placed: Some(wards),
                sen_placed: Some(0),
            },
        };
    *writer.farm_stats.lock().expect("farm stats lock") = vec![
        farm_member(1, 1, 220, 0),
        farm_member(2, 2, 180, 0),
        farm_member(3, 3, 140, 1),
        farm_member(4, 3, 20, 2),
        farm_member(5, 1, 25, 0),
        farm_member(6, 1, 220, 0),
        farm_member(7, 2, 180, 0),
        farm_member(8, 3, 140, 1),
        farm_member(9, 3, 20, 2),
        farm_member(10, 1, 25, 0),
    ];
    let service =
        MatchEnrichmentService::new(matches, players, api, writer.clone(), None::<FakeOpenSkill>);

    let result = service.enrich_match(service_request());

    assert!(result.success);
    assert_eq!(result.positions_estimated, 10);
    let stored = writer
        .stored_estimates
        .lock()
        .expect("stored estimates lock");
    assert_eq!(stored.len(), 10);
    assert!(stored.contains(&(1, "1".to_owned())));
    assert!(stored.contains(&(2, "2".to_owned())));
    assert!(stored.contains(&(6, "1".to_owned())));
    assert!(stored.contains(&(7, "2".to_owned())));
}

#[test]
fn test_enrich_match_position_estimate_failure_does_not_fail_enrichment() {
    let (matches, players, api, writer, _) = one_player_enrichment_fixture();
    api.set_details(
        ValveMatchId(8_181_518_332),
        vec![Ok(Some(enrichment_details(vec![enrichment_player(
            12_345,
        )])))],
    );
    // Only three rows for team 1 — not a valid five-player team, so the
    // heuristic can't resolve it. Enrichment must still succeed.
    *writer.farm_stats.lock().expect("farm stats lock") = vec![
        PositionFarmRow {
            team_number: 1,
            stats: ParticipantFarmStats {
                discord_id: 1,
                lane_role: Some(1),
                last_hits: Some(220),
                net_worth: None,
                gpm: None,
                obs_placed: Some(0),
                sen_placed: Some(0),
            },
        },
        PositionFarmRow {
            team_number: 1,
            stats: ParticipantFarmStats {
                discord_id: 2,
                lane_role: Some(2),
                last_hits: Some(180),
                net_worth: None,
                gpm: None,
                obs_placed: Some(0),
                sen_placed: Some(0),
            },
        },
    ];
    let service =
        MatchEnrichmentService::new(matches, players, api, writer.clone(), None::<FakeOpenSkill>);

    let result = service.enrich_match(service_request());

    assert!(result.success);
    assert_eq!(result.positions_estimated, 0);
    assert!(
        writer
            .stored_estimates
            .lock()
            .expect("stored estimates lock")
            .is_empty()
    );
}

#[test]
fn test_zero_matched_participants_is_a_failure_and_writes_nothing() {
    let (matches, players, api, writer, _) = one_player_enrichment_fixture();
    api.set_details(
        ValveMatchId(8_181_518_332),
        vec![Ok(Some(enrichment_details(vec![enrichment_player(
            99_999,
        )])))],
    );
    let service =
        MatchEnrichmentService::new(matches, players, api, writer.clone(), None::<FakeOpenSkill>);

    let result = service.enrich_match(service_request());

    assert!(!result.success);
    assert_eq!(result.players_enriched, 0);
    assert!(writer.writes.lock().expect("writes lock").is_empty());
}

#[test]
fn test_one_matched_participant_still_enriches() {
    let (matches, players, api, writer, _) = one_player_enrichment_fixture();
    api.set_details(
        ValveMatchId(8_181_518_332),
        vec![Ok(Some(enrichment_details(vec![enrichment_player(
            12_345,
        )])))],
    );
    let service =
        MatchEnrichmentService::new(matches, players, api, writer.clone(), None::<FakeOpenSkill>);

    let result = service.enrich_match(service_request());

    assert!(result.success);
    assert_eq!(result.players_enriched, 1);
    let writes = writer.writes.lock().expect("writes lock");
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].participant_updates.len(), 1);
}

#[test]
fn test_enrich_match_projects_wrapped_facts_for_every_participant() {
    let matches = FakeMatchRepository::default();
    matches.insert(
        GUILD,
        internal_match(1, MATCH_TIME),
        vec![
            InternalParticipant {
                discord_id: UserId(100),
                side: TeamSide::Radiant,
            },
            InternalParticipant {
                discord_id: UserId(200),
                side: TeamSide::Dire,
            },
        ],
    );
    let players = FakePlayerRepository::default();
    players.link(GUILD, UserId(100), vec![SteamId(12_345)]);
    players.link(GUILD, UserId(200), vec![SteamId(67_890)]);
    let mut matched_player = enrichment_player(12_345);
    matched_player.wrapped = WrappedPlayerTelemetry {
        actions_per_min: Some(321),
        courier_kills: Some(2),
        pings: Some(44),
        lane_role: Some(2),
        purchase_keys: vec![
            "rapier".to_owned(),
            "ward_observer".to_owned(),
            "rapier".to_owned(),
        ],
    };
    let mut details = enrichment_details(vec![matched_player]);
    details.comeback = Some(9_000);
    details.throw_amount = Some(4_000);
    let api = FakeOpenDota::default();
    api.set_details(ValveMatchId(8_181_518_332), vec![Ok(Some(details))]);
    let writer = RecordingWriter::default();
    let service = MatchEnrichmentService::new(
        matches,
        players.clone(),
        api,
        writer.clone(),
        None::<FakeOpenSkill>,
    );

    let result = service.enrich_match(service_request());

    assert!(result.success);
    assert_eq!(result.players_enriched, 1);
    assert_eq!(
        players.calls.lock().expect("steam lookup calls").as_slice(),
        &[(vec![UserId(100), UserId(200)], GUILD)]
    );
    let writes = writer.writes.lock().expect("writes lock");
    assert_eq!(writes[0].guild_id, GUILD);
    assert_eq!(
        writes[0].wrapped_facts,
        [
            WrappedEnrichmentFact {
                discord_id: UserId(100),
                actions_per_min: Some(321),
                courier_kills: Some(2),
                pings: Some(44),
                rapier_count: 2,
                lane_role: Some(2),
                comeback: Some(9_000),
                throw_amount: Some(4_000),
            },
            WrappedEnrichmentFact {
                discord_id: UserId(200),
                actions_per_min: None,
                courier_kills: None,
                pings: None,
                rapier_count: 0,
                lane_role: None,
                comeback: Some(9_000),
                throw_amount: Some(4_000),
            },
        ]
    );
}

#[test]
fn test_enrich_match_api_failure() {
    let (matches, players, api, writer, _) = one_player_enrichment_fixture();
    let service = MatchEnrichmentService::new(matches, players, api, writer, None::<FakeOpenSkill>);

    let result = service.enrich_match(service_request());

    assert!(!result.success);
    assert!(
        result
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Failed to fetch"))
    );
}

#[test]
fn test_enrich_match_reuses_preloaded_opendota_payload() {
    let (matches, players, api, writer, _) = one_player_enrichment_fixture();
    let service =
        MatchEnrichmentService::new(matches, players, api.clone(), writer, None::<FakeOpenSkill>);
    let mut request = service_request();
    request.opendota_match_data = Some(enrichment_details(vec![enrichment_player(12_345)]));

    assert!(service.enrich_match(request).success);
    assert_eq!(api.detail_call_count(), 0);
}

#[test]
fn test_enrich_match_player_not_found() {
    let (matches, players, api, writer, _) = one_player_enrichment_fixture();
    api.set_details(
        ValveMatchId(8_181_518_332),
        vec![Ok(Some(enrichment_details(vec![enrichment_player(
            99_999,
        )])))],
    );
    let service =
        MatchEnrichmentService::new(matches, players, api, writer.clone(), None::<FakeOpenSkill>);

    let result = service.enrich_match(service_request());

    assert!(!result.success);
    assert_eq!(result.players_enriched, 0);
    assert_eq!(result.players_not_found, [SteamId(12_345)]);
    assert!(writer.writes.lock().expect("writes lock").is_empty());
}

#[test]
fn test_enrich_match_no_steam_id() {
    let (matches, _, api, writer, _) = one_player_enrichment_fixture();
    api.set_details(
        ValveMatchId(8_181_518_332),
        vec![Ok(Some(enrichment_details(Vec::new())))],
    );
    let service = MatchEnrichmentService::new(
        matches,
        FakePlayerRepository::default(),
        api,
        writer.clone(),
        None::<FakeOpenSkill>,
    );

    let result = service.enrich_match(service_request());

    assert!(!result.success);
    assert_eq!(result.players_enriched, 0);
    assert!(result.players_not_found.is_empty());
    assert!(writer.writes.lock().expect("writes lock").is_empty());
}

#[test]
fn test_backfill_steam_ids() {
    let repository = FakeBackfillRepository::default();
    *repository.candidates.lock().expect("candidates lock") = vec![
        SteamIdBackfillCandidate {
            discord_id: UserId(100),
            dotabuff_url: "https://www.dotabuff.com/players/76561198012345678".to_owned(),
        },
        SteamIdBackfillCandidate {
            discord_id: UserId(101),
            dotabuff_url: "https://www.dotabuff.com/players/76561198087654321".to_owned(),
        },
    ];
    *repository.outcomes.lock().expect("outcomes lock") = vec![
        BulkSteamIdLinkOutcome {
            success: true,
            is_primary: true,
            error: None,
        },
        BulkSteamIdLinkOutcome {
            success: true,
            is_primary: true,
            error: None,
        },
    ];
    let extractor = FakeDotabuffExtractor::default();
    *extractor.responses.lock().expect("responses lock") =
        [Some(SteamId(52_079_950)), Some(SteamId(127_388_593))].into();
    let service = MatchEnrichmentService::new((), repository.clone(), extractor, (), None::<()>);

    let result = service.backfill_steam_ids().unwrap();

    assert_eq!(result.players_updated, 2);
    assert!(result.players_failed.is_empty());
    assert_eq!(
        repository.calls.lock().expect("bulk calls lock").as_slice(),
        &[vec![
            (UserId(100), SteamId(52_079_950)),
            (UserId(101), SteamId(127_388_593)),
        ]]
    );
}

#[test]
fn test_backfill_steam_ids_with_failures() {
    let repository = FakeBackfillRepository::default();
    *repository.candidates.lock().expect("candidates lock") = vec![
        SteamIdBackfillCandidate {
            discord_id: UserId(100),
            dotabuff_url: "valid".to_owned(),
        },
        SteamIdBackfillCandidate {
            discord_id: UserId(101),
            dotabuff_url: "invalid_url".to_owned(),
        },
    ];
    *repository.outcomes.lock().expect("outcomes lock") = vec![BulkSteamIdLinkOutcome {
        success: true,
        is_primary: true,
        error: None,
    }];
    let extractor = FakeDotabuffExtractor::default();
    *extractor.responses.lock().expect("responses lock") = [Some(SteamId(52_079_950)), None].into();
    let service = MatchEnrichmentService::new((), repository, extractor, (), None::<()>);

    assert_eq!(
        service.backfill_steam_ids().unwrap(),
        SteamIdBackfillResult {
            players_updated: 1,
            players_failed: vec![UserId(101)],
        }
    );
}

#[test]
fn test_backfill_steam_ids_preserves_failure_order_and_batch_conflicts() {
    let repository = FakeBackfillRepository::default();
    *repository.candidates.lock().expect("candidates lock") = vec![
        SteamIdBackfillCandidate {
            discord_id: UserId(10),
            dotabuff_url: "bad".to_owned(),
        },
        SteamIdBackfillCandidate {
            discord_id: UserId(11),
            dotabuff_url: "conflict".to_owned(),
        },
        SteamIdBackfillCandidate {
            discord_id: UserId(12),
            dotabuff_url: "ok".to_owned(),
        },
    ];
    *repository.outcomes.lock().expect("outcomes lock") = vec![
        BulkSteamIdLinkOutcome {
            success: false,
            is_primary: false,
            error: Some("Steam ID 500 is already linked to another player".to_owned()),
        },
        BulkSteamIdLinkOutcome {
            success: true,
            is_primary: true,
            error: None,
        },
    ];
    let extractor = FakeDotabuffExtractor::default();
    *extractor.responses.lock().expect("responses lock") =
        [None, Some(SteamId(500)), Some(SteamId(600))].into();
    let service = MatchEnrichmentService::new((), repository.clone(), extractor, (), None::<()>);

    assert_eq!(
        service.backfill_steam_ids().unwrap(),
        SteamIdBackfillResult {
            players_updated: 1,
            players_failed: vec![UserId(10), UserId(11)],
        }
    );
    assert_eq!(
        repository.calls.lock().expect("bulk calls lock").as_slice(),
        &[vec![(UserId(11), SteamId(500)), (UserId(12), SteamId(600)),]]
    );
}

#[test]
fn test_enrich_match_forwards_guild_id_to_openskill_update() {
    let target_guild = GuildId(9_876_543_210);
    let matches = FakeMatchRepository::default();
    matches.insert(
        target_guild,
        internal_match(1, MATCH_TIME),
        vec![InternalParticipant {
            discord_id: UserId(100),
            side: TeamSide::Radiant,
        }],
    );
    let players = FakePlayerRepository::default();
    players.link(target_guild, UserId(100), vec![SteamId(12_345)]);
    let api = FakeOpenDota::default();
    api.set_details(
        ValveMatchId(8_181_518_332),
        vec![Ok(Some(enrichment_details(vec![enrichment_player(
            12_345,
        )])))],
    );
    let openskill = FakeOpenSkill::default();
    let service = MatchEnrichmentService::new(
        matches,
        players,
        api,
        RecordingWriter::default(),
        Some(openskill.clone()),
    );
    let mut request = service_request();
    request.guild_id = Some(target_guild);

    assert!(service.enrich_match(request).success);
    assert_eq!(
        openskill
            .calls
            .lock()
            .expect("openskill calls lock")
            .as_slice(),
        &[(InternalMatchId(1), Some(target_guild))]
    );
}

#[test]
fn stronger_concurrent_live_discovery_is_single_flight_and_idempotent() {
    let (service, _, _, api, enrichment, _) = setup_single();
    api.set_default_history(Ok(Some(history(99_999, MATCH_TIME))));
    api.block_histories();
    let service = Arc::new(service);
    let first_service = Arc::clone(&service);
    let first =
        thread::spawn(move || first_service.discover_match(InternalMatchId(1), Some(GUILD), false));
    api.wait_until_history_entered();

    let concurrent = service.discover_match(InternalMatchId(1), Some(GUILD), false);
    assert_eq!(concurrent.status, DiscoveryStatus::InProgress);

    api.release_histories();
    let completed = first.join().expect("discovery thread");
    assert_eq!(completed.status, DiscoveryStatus::Discovered);
    let calls_after_completion = api.history_call_count();
    let repeated = service.discover_match(InternalMatchId(1), Some(GUILD), false);
    assert_eq!(repeated, completed);
    assert_eq!(api.history_call_count(), calls_after_completion);
    assert_eq!(enrichment.requests.lock().expect("requests lock").len(), 1);
}

#[test]
fn stronger_completed_runs_are_isolated_by_signed_guild() {
    let (service, matches, players, api, enrichment, _) = setup_single();
    let signed_guild = GuildId(-42);
    matches.insert(signed_guild, internal_match(1, MATCH_TIME), participants());
    for id in 1..=10 {
        players.link(signed_guild, UserId(id), vec![SteamId(id + 2_000)]);
    }
    api.set_default_history(Ok(Some(history(-99_999, MATCH_TIME))));

    let first = service.discover_match(InternalMatchId(1), Some(GUILD), false);
    let second = service.discover_match(InternalMatchId(1), Some(signed_guild), false);

    assert_eq!(first.status, DiscoveryStatus::Discovered);
    assert_eq!(second.status, DiscoveryStatus::Discovered);
    let requests = enrichment.requests.lock().expect("requests lock");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].guild_id, GUILD);
    assert_eq!(requests[1].guild_id, signed_guild);
}

#[test]
fn stronger_transient_failures_are_retryable_but_successes_are_cached() {
    let (service, _, _, api, _, _) = setup_single();
    api.set_history(
        SteamId(1_001),
        vec![
            Err(DiscoveryPortError::new("temporary")),
            Ok(Some(history(99_999, MATCH_TIME))),
        ],
    );
    api.set_default_history(Ok(Some(history(99_999, MATCH_TIME))));

    let first = service.discover_match(InternalMatchId(1), Some(GUILD), true);
    let second = service.discover_match(InternalMatchId(1), Some(GUILD), true);

    assert_eq!(first.status, DiscoveryStatus::LowConfidence);
    assert_eq!(second.status, DiscoveryStatus::Discovered);
}

#[test]
fn stronger_parse_time_rejects_non_finite_unix_values() {
    assert_eq!(parse_match_time(&MatchTimeValue::Unix(f64::NAN)), None);
    assert_eq!(parse_match_time(&MatchTimeValue::Unix(f64::INFINITY)), None);
}

#[test]
fn stronger_roster_count_deduplicates_multiple_accounts_per_player() {
    let details =
        OpenDotaMatchDetails::roster(ValveMatchId(1), [SteamId(10), SteamId(11), SteamId(20)]);
    let participants = vec![
        InternalParticipant {
            discord_id: UserId(1),
            side: TeamSide::Radiant,
        },
        InternalParticipant {
            discord_id: UserId(2),
            side: TeamSide::Dire,
        },
    ];
    let steam_ids = BTreeMap::from([
        (UserId(1), vec![SteamId(10), SteamId(11)]),
        (UserId(2), vec![SteamId(20)]),
    ]);

    assert_eq!(count_roster_matches(&details, &participants, &steam_ids), 2);
}
