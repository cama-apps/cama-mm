//! Pick up parsed replay data for recent matches without an admin command.
//!
//! Enrichment stores whatever OpenDota has moments after a match, which is
//! almost never the parsed replay, and positions are derived only from parsed
//! lane and farm data. Each wake re-fetches a few recent matches still missing
//! it; the enrichment service requests the parse itself when a fetch comes
//! back unparsed.

use super::*;
use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub(super) const PARSED_REFRESH_WORKER_NAME: &str = "parsed_stats_refresh";
pub(super) const PARSED_REFRESH_WAKE_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// OpenDota can only parse a replay while Valve still serves it, so older
/// matches are left to the manual refresh.
const PARSED_REFRESH_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Two hours of wakes; a match still unparsed after that is not retried.
const PARSED_REFRESH_MAX_ATTEMPTS: i64 = 12;
const PARSED_REFRESH_BATCH: usize = 5;

struct ParsedRefreshWorker {
    handler: Arc<EnrichmentHandler>,
}

pub(super) fn spec(handler: Arc<EnrichmentHandler>) -> BackgroundWorkerSpec {
    BackgroundWorkerSpec::new(
        PARSED_REFRESH_WORKER_NAME,
        Arc::new(ParsedRefreshWorker { handler }),
    )
}

impl EnrichmentHandler {
    /// Refresh one batch of recent matches, returning how many now have
    /// complete position inputs.
    pub(super) async fn refresh_recent_parsed_stats(&self) -> Result<usize, String> {
        // A manual refresh already covers these matches; overlapping it would
        // only spend quota twice.
        let Some(_guard) = try_claim_parsed_refresh(&self.parsed_refresh_running) else {
            return Ok(0);
        };
        let enrichment = Arc::clone(&self.enrichment);
        let matches = self.matches.clone();
        let opendota = Arc::clone(&self.opendota);
        let refresh_interval = self.parsed_refresh_interval;
        run_blocking(move || {
            let candidates = matches
                .recent_matches_missing_role_inputs(
                    PARSED_REFRESH_MAX_AGE,
                    PARSED_REFRESH_MAX_ATTEMPTS,
                    PARSED_REFRESH_BATCH,
                )
                .map_err(|error| error.to_string())?;
            let mut completed = 0;
            for (guild_id, match_id, valve_match_id) in candidates {
                let quota = opendota
                    .quota_snapshot_blocking()
                    .map_err(|error| error.to_string())?;
                if quota.request_is_blocked() {
                    break;
                }
                let result =
                    refresh_linked_match(&enrichment, &matches, guild_id, match_id, valve_match_id);
                matches
                    .mark_parsed_refresh_attempt(match_id, Some(guild_id))
                    .map_err(|error| error.to_string())?;
                if result.success
                    && matches
                        .parsed_stats_coverage(match_id, Some(guild_id))
                        .is_ok_and(parsed_stats_complete)
                {
                    completed += 1;
                }
                if !refresh_interval.is_zero() {
                    std::thread::sleep(refresh_interval);
                }
            }
            Ok::<_, String>(completed)
        })
        .await
    }
}

#[async_trait]
impl BackgroundWorker for ParsedRefreshWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            if context.shutdown_requested() {
                return Ok(());
            }

            match self.handler.refresh_recent_parsed_stats().await {
                Ok(0) => {}
                Ok(completed) => info!(completed, "parsed stats arrived for recent matches"),
                Err(error) => warn!(%error, "automatic parsed-stats refresh failed"),
            }

            if !context.sleep(PARSED_REFRESH_WAKE_INTERVAL).await {
                return Ok(());
            }
        }
    }
}
