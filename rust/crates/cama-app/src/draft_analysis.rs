//! Optional post-match draft estimates and independently verified drafter identities.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cama_db::match_draft::{MatchDraftAnalysis, MatchDraftRepository};
use cama_domain::draft_analysis::draft_winner;

use crate::dedicated_lobby_channel::UserId;
use crate::draft_analysis_http::{BatruDraftClient, HeroDraft};
use crate::match_discovery::{OpenDotaMatchDetails, SteamId};

pub trait DraftPredictionPort: Send + Sync {
    fn predict(&self, draft: &HeroDraft) -> Result<i64, String>;

    fn retry_after(&self) -> Option<Duration> {
        None
    }
}

impl DraftPredictionPort for BatruDraftClient {
    fn predict(&self, draft: &HeroDraft) -> Result<i64, String> {
        BatruDraftClient::predict(self, draft).map_err(|error| error.to_string())
    }

    fn retry_after(&self) -> Option<Duration> {
        BatruDraftClient::retry_after(self)
    }
}

pub struct DraftAnalysisService {
    repo: MatchDraftRepository,
    predictor: Arc<dyn DraftPredictionPort>,
}

impl crate::match_discovery::DraftEnrichmentPort for DraftAnalysisService {
    fn enrich_draft(
        &self,
        match_id: i64,
        guild_id: i64,
        details: &OpenDotaMatchDetails,
        discord_to_steam_ids: &BTreeMap<UserId, Vec<SteamId>>,
    ) -> Result<(), String> {
        self.enrich(match_id, guild_id, details, discord_to_steam_ids)
    }

    fn retry_after(&self) -> Option<Duration> {
        DraftAnalysisService::retry_after(self)
    }
}

impl DraftAnalysisService {
    #[must_use]
    pub fn new(repo: MatchDraftRepository, predictor: Arc<dyn DraftPredictionPort>) -> Self {
        Self { repo, predictor }
    }

    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        self.predictor.retry_after()
    }

    /// Run on a blocking worker, after the match's OpenDota link has been saved.
    /// Captain metadata is saved even when the optional prediction fails.
    pub fn enrich(
        &self,
        match_id: i64,
        guild_id: i64,
        details: &OpenDotaMatchDetails,
        discord_to_steam_ids: &BTreeMap<UserId, Vec<SteamId>>,
    ) -> Result<(), String> {
        let existing = self
            .repo
            .get(match_id, guild_id)
            .map_err(|e| e.to_string())?;
        let (radiant_steam, mut radiant_discord) =
            captain(details, details.radiant_captain, true, discord_to_steam_ids);
        let (dire_steam, mut dire_discord) =
            captain(details, details.dire_captain, false, discord_to_steam_ids);
        // Alternate Steam accounts can link to one Discord member. That does
        // not establish which person drafted when both captains resolve there.
        if radiant_discord.is_some() && radiant_discord == dire_discord {
            radiant_discord = None;
            dire_discord = None;
        }
        let mut analysis = MatchDraftAnalysis {
            match_id,
            guild_id,
            valve_match_id: details.match_id.0,
            radiant_drafter_steam_id: radiant_steam,
            dire_drafter_steam_id: dire_steam,
            radiant_drafter_discord_id: radiant_discord,
            dire_drafter_discord_id: dire_discord,
            drafter_source: (radiant_steam.is_some() || dire_steam.is_some())
                .then(|| "opendota".to_owned()),
            ..MatchDraftAnalysis::default()
        };
        let draft_result = hero_draft(details).map(|draft| {
            let heroes_json = serde_json::json!({
                "radiant": draft.radiant,
                "dire": draft.dire,
            })
            .to_string();
            (draft, heroes_json)
        });
        // A known composition change invalidates the old estimate even when the
        // provider is unavailable. Compare the stored input atomically so a
        // concurrent correction cannot have its newer prediction cleared.
        if let Ok((_, heroes_json)) = &draft_result
            && let Some(old) = &existing
            && old.valve_match_id == details.match_id.0
            && let Some(old_heroes_json) = old.draft_heroes_json.as_deref()
            && old_heroes_json != heroes_json
            && !self
                .repo
                .clear_prediction(match_id, guild_id, details.match_id.0, old_heroes_json)
                .map_err(|error| error.to_string())?
        {
            return Err("draft analysis changed while refreshing its hero composition".to_owned());
        }
        let prediction_result = (|| {
            let (draft, heroes_json) = draft_result?;
            if existing.as_ref().is_some_and(|old| {
                old.valve_match_id == details.match_id.0
                    && old.radiant_win_probability_bps.is_some()
                    && old.prediction_provider.as_deref() == Some("batru")
                    && old.draft_heroes_json.as_deref() == Some(heroes_json.as_str())
            }) {
                return Ok(());
            }
            let probability = self.predictor.predict(&draft)?;
            let winner = draft_winner(probability)
                .ok_or_else(|| "draft provider returned an invalid probability".to_owned())?;
            let recorded_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|duration| i64::try_from(duration.as_secs()).ok())
                .ok_or_else(|| "draft prediction timestamp unavailable".to_owned())?;
            analysis.radiant_win_probability_bps = Some(probability);
            analysis.draft_winner = Some(winner);
            analysis.prediction_provider = Some("batru".to_owned());
            analysis.prediction_recorded_at = Some(recorded_at);
            analysis.draft_heroes_json = Some(heroes_json);
            Ok(())
        })();
        if !self.repo.save(&analysis).map_err(|e| e.to_string())? {
            return Err("draft analysis match link changed or is unavailable".to_owned());
        }
        prediction_result
    }
}

fn hero_draft(details: &OpenDotaMatchDetails) -> Result<HeroDraft, String> {
    let invalid =
        || "draft estimate requires ten distinct heroes and valid player slots".to_owned();
    if details.players.len() != 10 {
        return Err(invalid());
    }
    let mut slots = BTreeSet::new();
    let mut heroes = BTreeSet::new();
    let mut draft = HeroDraft {
        radiant: Vec::new(),
        dire: Vec::new(),
    };
    for player in &details.players {
        let slot = player.player_slot.ok_or_else(invalid)?;
        let hero = player.stats.hero_id;
        if !slots.insert(slot) || hero <= 0 || !heroes.insert(hero) {
            return Err(invalid());
        }
        match slot {
            0..=4 => draft.radiant.push(hero),
            128..=132 => draft.dire.push(hero),
            _ => return Err(invalid()),
        }
    }
    if draft.radiant.len() != 5 || draft.dire.len() != 5 {
        return Err(invalid());
    }
    draft.radiant.sort_unstable();
    draft.dire.sort_unstable();
    Ok(draft)
}

fn captain(
    details: &OpenDotaMatchDetails,
    steam_id: Option<SteamId>,
    radiant: bool,
    discord_to_steam_ids: &BTreeMap<UserId, Vec<SteamId>>,
) -> (Option<i64>, Option<i64>) {
    let Some(steam_id) = steam_id.filter(|id| (1..i64::from(u32::MAX)).contains(&id.0)) else {
        return (None, None);
    };
    let mut roster_matches = details
        .players
        .iter()
        .filter(|player| player.account_id == Some(steam_id));
    let Some(player) = roster_matches.next() else {
        return (None, None);
    };
    if roster_matches.next().is_some()
        || !matches!(
            (radiant, player.player_slot),
            (true, Some(0..=4)) | (false, Some(128..=132))
        )
        || details
            .players
            .iter()
            .filter(|other| other.player_slot == player.player_slot)
            .count()
            != 1
    {
        return (None, None);
    }
    let mut discord_matches = discord_to_steam_ids
        .iter()
        .filter(|(discord_id, steam_ids)| discord_id.0 > 0 && steam_ids.contains(&steam_id))
        .map(|(discord_id, _)| discord_id.0);
    let discord_id = discord_matches.next();
    (
        Some(steam_id.0),
        if discord_matches.next().is_none() {
            discord_id
        } else {
            None
        },
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::match_discovery::ValveMatchId;
    use crate::test_support::{FastTestDatabase, fast_migrated_database};

    use super::*;

    struct FakePrediction {
        calls: Mutex<Vec<HeroDraft>>,
        result: Mutex<Result<i64, String>>,
    }

    impl DraftPredictionPort for FakePrediction {
        fn predict(&self, draft: &HeroDraft) -> Result<i64, String> {
            self.calls.lock().unwrap().push(draft.clone());
            self.result.lock().unwrap().clone()
        }
    }

    struct Fixture {
        _database: FastTestDatabase,
        repo: MatchDraftRepository,
        service: DraftAnalysisService,
        predictor: Arc<FakePrediction>,
        details: OpenDotaMatchDetails,
        mapping: BTreeMap<UserId, Vec<SteamId>>,
    }

    impl Fixture {
        fn new() -> Self {
            let database = fast_migrated_database();
            cama_db::open_runtime_connection(database.path())
                .unwrap()
                .execute(
                    "INSERT INTO matches(match_id,guild_id,valve_match_id,team1_players,team2_players)
                     VALUES (1,10,100,'[]','[]')",
                    [],
                )
                .unwrap();
            let repo = MatchDraftRepository::new(database.path());
            let predictor = Arc::new(FakePrediction {
                calls: Mutex::new(Vec::new()),
                result: Mutex::new(Ok(6_166)),
            });
            let service = DraftAnalysisService::new(repo.clone(), predictor.clone());
            let mut details =
                OpenDotaMatchDetails::roster(ValveMatchId(100), (1001..=1010).map(SteamId));
            for (index, player) in details.players.iter_mut().enumerate() {
                player.stats.hero_id = i64::try_from(index + 1).unwrap();
            }
            details.game_mode = 2;
            details.radiant_captain = Some(SteamId(1005));
            details.dire_captain = Some(SteamId(1008));
            let mapping = (1..=10)
                .map(|id| (UserId(id), vec![SteamId(1000 + id)]))
                .collect();
            Self {
                _database: database,
                repo,
                service,
                predictor,
                details,
                mapping,
            }
        }

        fn enrich(&self) -> Result<(), String> {
            self.service.enrich(1, 10, &self.details, &self.mapping)
        }

        fn saved(&self) -> MatchDraftAnalysis {
            self.repo.get(1, 10).unwrap().unwrap()
        }
    }

    #[test]
    fn retry_delay_delegates_without_requesting_a_prediction() {
        struct ThrottledPrediction;
        impl DraftPredictionPort for ThrottledPrediction {
            fn predict(&self, _draft: &HeroDraft) -> Result<i64, String> {
                panic!("checking quota must not predict");
            }
            fn retry_after(&self) -> Option<Duration> {
                Some(Duration::from_secs(7))
            }
        }
        let fixture = Fixture::new();
        assert_eq!(fixture.service.retry_after(), None);
        assert!(fixture.predictor.calls.lock().unwrap().is_empty());
        let service = DraftAnalysisService::new(fixture.repo, Arc::new(ThrottledPrediction));
        assert_eq!(service.retry_after(), Some(Duration::from_secs(7)));
        assert_eq!(
            crate::match_discovery::DraftEnrichmentPort::retry_after(&service),
            Some(Duration::from_secs(7))
        );
    }

    #[test]
    fn captain_identity_uses_explicit_accounts_and_player_sides() {
        let mut fixture = Fixture::new();
        fixture.details.players.reverse();
        fixture.enrich().unwrap();
        let saved = fixture.saved();
        assert_eq!(saved.radiant_drafter_steam_id, Some(1005));
        assert_eq!(saved.dire_drafter_steam_id, Some(1008));
        assert_eq!(saved.radiant_drafter_discord_id, Some(5));
        assert_eq!(saved.dire_drafter_discord_id, Some(8));
        assert_eq!(saved.drafter_source.as_deref(), Some("opendota"));
        assert_eq!(saved.radiant_win_probability_bps, Some(6166));
        assert_eq!(saved.draft_winner, Some(1));
        assert_eq!(saved.prediction_provider.as_deref(), Some("batru"));
        assert_eq!(
            fixture.predictor.calls.lock().unwrap()[0],
            HeroDraft {
                radiant: vec![1, 2, 3, 4, 5],
                dire: vec![6, 7, 8, 9, 10],
            }
        );
    }

    #[test]
    fn missing_wrong_side_and_ambiguous_captains_remain_unknown() {
        let mut fixture = Fixture::new();
        fixture.details.radiant_captain = Some(SteamId(1008));
        fixture.details.dire_captain = None;
        fixture.enrich().unwrap();
        let saved = fixture.saved();
        assert_eq!(saved.radiant_drafter_steam_id, None);
        assert_eq!(saved.dire_drafter_steam_id, None);
        assert_eq!(saved.radiant_drafter_discord_id, None);
        assert_eq!(saved.dire_drafter_discord_id, None);

        let mut fixture = Fixture::new();
        fixture.mapping.insert(UserId(99), vec![SteamId(1005)]);
        fixture.details.players[9].account_id = Some(SteamId(1008));
        fixture.enrich().unwrap();
        let saved = fixture.saved();
        assert_eq!(saved.radiant_drafter_steam_id, Some(1005));
        assert_eq!(saved.radiant_drafter_discord_id, None);
        assert_eq!(saved.dire_drafter_steam_id, None);
    }

    #[test]
    fn opposing_captains_linked_to_one_discord_member_remain_unattributed() {
        let mut fixture = Fixture::new();
        fixture.mapping.remove(&UserId(8));
        fixture
            .mapping
            .get_mut(&UserId(5))
            .unwrap()
            .push(SteamId(1008));
        fixture.enrich().unwrap();
        let saved = fixture.saved();
        assert_eq!(saved.radiant_drafter_steam_id, Some(1005));
        assert_eq!(saved.dire_drafter_steam_id, Some(1008));
        assert_eq!(saved.radiant_drafter_discord_id, None);
        assert_eq!(saved.dire_drafter_discord_id, None);
        assert_eq!(saved.radiant_win_probability_bps, Some(6166));
    }

    #[test]
    fn prediction_failure_still_saves_captains_and_retries_missing_prediction() {
        let fixture = Fixture::new();
        *fixture.predictor.result.lock().unwrap() = Err("offline".into());
        assert_eq!(fixture.enrich(), Err("offline".into()));
        let saved = fixture.saved();
        assert_eq!(saved.radiant_drafter_discord_id, Some(5));
        assert_eq!(saved.radiant_win_probability_bps, None);
        assert_eq!(saved.draft_winner, None);
        *fixture.predictor.result.lock().unwrap() = Ok(4896);
        fixture.enrich().unwrap();
        assert_eq!(fixture.saved().radiant_win_probability_bps, Some(4896));
        assert_eq!(fixture.saved().draft_winner, Some(0));
        assert_eq!(fixture.predictor.calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn cached_prediction_preserves_timestamp_and_fills_missing_discord_mapping() {
        let mut fixture = Fixture::new();
        fixture.mapping.remove(&UserId(5));
        fixture.enrich().unwrap();
        let mut saved = fixture.saved();
        saved.prediction_recorded_at = Some(123);
        fixture.repo.save(&saved).unwrap();
        fixture.mapping.insert(UserId(5), vec![SteamId(1005)]);
        fixture.details.players.reverse();
        fixture.enrich().unwrap();
        assert_eq!(fixture.predictor.calls.lock().unwrap().len(), 1);
        assert_eq!(fixture.saved().prediction_recorded_at, Some(123));
        assert_eq!(fixture.saved().radiant_drafter_discord_id, Some(5));

        fixture.details.players[0].stats.hero_id = 11;
        fixture.enrich().unwrap();
        assert_eq!(fixture.predictor.calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn corrected_heroes_clear_stale_prediction_before_a_failed_refresh() {
        let mut fixture = Fixture::new();
        fixture.enrich().unwrap();
        fixture.details.players[0].stats.hero_id = 11;
        *fixture.predictor.result.lock().unwrap() = Err("offline".into());
        assert_eq!(fixture.enrich(), Err("offline".into()));
        let saved = fixture.saved();
        assert_eq!(saved.radiant_win_probability_bps, None);
        assert_eq!(saved.draft_winner, None);
        assert_eq!(saved.prediction_provider, None);
        assert_eq!(saved.prediction_recorded_at, None);
        assert_eq!(saved.draft_heroes_json, None);
        assert_eq!(saved.radiant_drafter_discord_id, Some(5));
        assert_eq!(saved.dire_drafter_discord_id, Some(8));
        assert_eq!(
            fixture
                .repo
                .backfill_candidates(10, 10, None)
                .unwrap()
                .len(),
            1
        );

        *fixture.predictor.result.lock().unwrap() = Ok(3800);
        fixture.enrich().unwrap();
        let saved = fixture.saved();
        assert_eq!(saved.radiant_win_probability_bps, Some(3800));
        assert_eq!(saved.draft_winner, Some(2));
        assert_eq!(fixture.predictor.calls.lock().unwrap().len(), 3);
        let heroes: serde_json::Value =
            serde_json::from_str(saved.draft_heroes_json.as_deref().unwrap()).unwrap();
        assert_eq!(heroes["radiant"], serde_json::json!([2, 3, 4, 5, 11]));
    }

    #[test]
    fn invalid_heroes_and_slots_never_reach_predictor() {
        for mutation in 0..4 {
            let mut fixture = Fixture::new();
            match mutation {
                0 => {
                    fixture.details.players.pop();
                }
                1 => fixture.details.players[0].stats.hero_id = 0,
                2 => fixture.details.players[0].stats.hero_id = 2,
                _ => fixture.details.players[0].player_slot = Some(5),
            }
            assert!(fixture.enrich().is_err());
            assert!(fixture.predictor.calls.lock().unwrap().is_empty());
            assert_eq!(fixture.saved().radiant_win_probability_bps, None);
            assert_eq!(fixture.saved().draft_winner, None);
        }
    }

    #[test]
    fn invalid_provider_probability_cannot_be_stored_as_a_prediction() {
        let fixture = Fixture::new();
        *fixture.predictor.result.lock().unwrap() = Ok(10001);
        assert!(fixture.enrich().is_err());
        assert_eq!(fixture.saved().radiant_win_probability_bps, None);
        assert_eq!(fixture.saved().radiant_drafter_discord_id, Some(5));
    }

    #[test]
    fn relinked_valve_match_does_not_reuse_old_prediction_or_captains() {
        let mut fixture = Fixture::new();
        fixture.enrich().unwrap();
        cama_db::open_runtime_connection(fixture._database.path())
            .unwrap()
            .execute("UPDATE matches SET valve_match_id=200 WHERE match_id=1", [])
            .unwrap();
        fixture.details.match_id = ValveMatchId(200);
        fixture.details.radiant_captain = None;
        fixture.details.dire_captain = None;
        *fixture.predictor.result.lock().unwrap() = Ok(4896);
        fixture.enrich().unwrap();
        let saved = fixture.saved();
        assert_eq!(fixture.predictor.calls.lock().unwrap().len(), 2);
        assert_eq!(saved.valve_match_id, 200);
        assert_eq!(saved.radiant_win_probability_bps, Some(4896));
        assert_eq!(saved.draft_winner, Some(0));
        assert_eq!(saved.radiant_drafter_steam_id, None);
        assert_eq!(saved.dire_drafter_discord_id, None);
    }
}
