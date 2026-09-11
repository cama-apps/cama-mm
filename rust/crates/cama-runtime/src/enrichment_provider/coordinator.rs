//! Enrich known hosted match IDs immediately; fill parsed statistics later.

use super::*;
use cama_app::match_discovery::{ValveMatchId, project_match_details, statistics_need_api};

fn enrichment_request(
    guild_id: i64,
    match_id: i64,
    valve_match_id: i64,
    details: Option<OpenDotaMatchDetails>,
) -> EnrichMatchRequest {
    EnrichMatchRequest {
        internal_match_id: InternalMatchId(match_id),
        valve_match_id: ValveMatchId(valve_match_id),
        guild_id: Some(cama_app::dedicated_lobby_channel::GuildId(guild_id)),
        source: EnrichmentSource::Auto,
        confidence: Some(1.0),
        skip_validation: false,
        opendota_match_data: details,
    }
}

fn discovery_result(
    match_id: i64,
    valve_match_id: i64,
    result: cama_app::match_discovery::MatchEnrichmentServiceResult,
) -> DiscoveryResult {
    DiscoveryResult {
        match_id: InternalMatchId(match_id),
        status: if result.success {
            DiscoveryStatus::Discovered
        } else if result.validation_error.is_some() {
            DiscoveryStatus::ValidationFailed
        } else {
            DiscoveryStatus::UpstreamUnavailable
        },
        valve_match_id: Some(ValveMatchId(valve_match_id)),
        confidence: Some(1.0),
        player_count: result.players_enriched,
        total_players: 10,
        players_with_steam_id: 10,
        validation_error: result.error,
    }
}

impl EnrichmentHandler {
    pub(super) async fn enrich_known_recorded_match(
        &self,
        guild_id: i64,
        match_id: i64,
        valve_match_id: i64,
        gc: Option<String>,
    ) -> Result<RecordedMatchDiscoveryOutcome, String> {
        if let Some(raw) = gc {
            let details = serde_json::from_str(&raw)
                .ok()
                .and_then(|value| project_match_details(value, valve_match_id));
            if let Some(details) = details {
                let enrichment = Arc::clone(&self.enrichment);
                let result = run_blocking(move || {
                    Ok::<_, String>(enrichment.enrich_match(enrichment_request(
                        guild_id,
                        match_id,
                        valve_match_id,
                        Some(details),
                    )))
                })
                .await?;
                if result.success {
                    let result = discovery_result(match_id, valve_match_id, result);
                    let mut response = self
                        .recorded_match_discovery_response(guild_id, &result)
                        .await?;
                    response.content = format!(
                        "📊 Match #{match_id} statistics from Dota. Use `/matches view` for updated stats and charts as they become available."
                    );
                    self.spawn_statistics_fallback(guild_id, match_id, valve_match_id);
                    return Ok(RecordedMatchDiscoveryOutcome::Discovered { result, response });
                }
                if result.validation_error.is_some() {
                    return Ok(RecordedMatchDiscoveryOutcome::Stopped(discovery_result(
                        match_id,
                        valve_match_id,
                        result,
                    )));
                }
            }
        }
        let mut last_result = None;
        // Known match IDs never need a player-history identity search.
        for delay in &self.enrichment_retry_delays {
            tokio::time::sleep(*delay).await;
            let enrichment = Arc::clone(&self.enrichment);
            let result = run_blocking(move || {
                Ok::<_, String>(enrichment.enrich_match(enrichment_request(
                    guild_id,
                    match_id,
                    valve_match_id,
                    None,
                )))
            })
            .await?;
            let result = discovery_result(match_id, valve_match_id, result);
            if result.status == DiscoveryStatus::Discovered {
                let response = self
                    .recorded_match_discovery_response(guild_id, &result)
                    .await?;
                self.spawn_statistics_fallback(guild_id, match_id, valve_match_id);
                return Ok(RecordedMatchDiscoveryOutcome::Discovered { result, response });
            }
            if result.status == DiscoveryStatus::ValidationFailed {
                return Ok(RecordedMatchDiscoveryOutcome::Stopped(result));
            }
            last_result = Some(result);
        }
        Ok(RecordedMatchDiscoveryOutcome::Exhausted { last_result })
    }

    fn spawn_statistics_fallback(&self, guild_id: i64, match_id: i64, valve_match_id: i64) {
        let matches = self.matches.clone();
        let enrichment = Arc::clone(&self.enrichment);
        let delays = self.enrichment_retry_delays.clone();
        tokio::spawn(async move {
            for delay in delays {
                tokio::time::sleep(delay).await;
                let matches = matches.clone();
                let enrichment = Arc::clone(&enrichment);
                let result = run_blocking(move || {
                    let raw = matches
                        .raw_enrichment_data(match_id, Some(guild_id))
                        .map_err(|e| e.to_string())?;
                    if raw.as_deref().is_some_and(|raw| !statistics_need_api(raw)) {
                        return Ok::<_, String>(true);
                    }
                    let result = enrichment.enrich_match(enrichment_request(
                        guild_id,
                        match_id,
                        valve_match_id,
                        None,
                    ));
                    if result.validation_error.is_some() {
                        return Err(result
                            .error
                            .unwrap_or_else(|| "Statistics validation failed".into()));
                    }
                    let raw = matches
                        .raw_enrichment_data(match_id, Some(guild_id))
                        .map_err(|e| e.to_string())?;
                    Ok(raw.as_deref().is_some_and(|raw| !statistics_need_api(raw)))
                })
                .await;
                match result {
                    Ok(true) => return,
                    Ok(false) => {}
                    Err(error) => {
                        warn!(guild_id, match_id, %error, "postgame statistics fallback stopped");
                        return;
                    }
                }
            }
            debug!(
                guild_id,
                match_id, "postgame statistics remain eligible for parsed-stat backfill"
            );
        });
    }
}
