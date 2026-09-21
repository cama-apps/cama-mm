//! Capture postgame material independently of private Discord delivery.
use super::*;

#[derive(Default)]
pub(super) struct Diagnostics {
    entries: BTreeMap<(i64, i64, &'static str), (String, i64)>,
}

impl SpectatorWorker {
    // State transitions are immediate; unchanged health is logged at most once
    // per five minutes. Never log live positions, scores, or player identities.
    pub(super) fn log_status(
        &self,
        identity: (i64, i64),
        component: &'static str,
        status: &str,
        now: i64,
    ) {
        let Ok(mut diagnostics) = self.diagnostics.lock() else {
            return;
        };
        diagnostics
            .entries
            .retain(|_, (_, at)| now.saturating_sub(*at) < 3600);
        let key = (identity.0, identity.1, component);
        if diagnostics
            .entries
            .get(&key)
            .is_some_and(|(previous, at)| previous == status && now.saturating_sub(*at) < 300)
        {
            return;
        }
        tracing::info!(
            guild_id = identity.0,
            pending_match_id = identity.1,
            component,
            status,
            "Dota spectator status"
        );
        diagnostics.entries.insert(key, (status.to_owned(), now));
    }

    pub(super) async fn observe_games(&self, games: &[(PendingMatchRecord, usize)], now: i64) {
        for (game, viewers) in games {
            let identity = (game.guild_id, game.pending_match_id);
            self.log_status(
                identity,
                "audience",
                &format!("{viewers} eligible viewers"),
                now,
            );
            if let Err(error) = self.capture_game(game, now).await {
                self.log_status(identity, "recap_capture", &format!("failed: {error}"), now);
            }
        }
    }

    async fn capture_game(&self, game: &PendingMatchRecord, now: i64) -> Result<(), String> {
        let identity = (game.guild_id, game.pending_match_id);
        if !game.state.betting_closed() || game.state.betting_open(now) {
            self.log_status(identity, "feed", "waiting for betting to close", now);
            return Ok(());
        }
        let participants = valid_roster(game)?;
        let Some(snapshot) = self.live.snapshot(game.guild_id, game.pending_match_id) else {
            self.log_status(identity, "feed", "waiting for live data", now);
            return Ok(());
        };
        if snapshot.stale {
            self.log_status(identity, "feed", "stale live data; capture paused", now);
            return Ok(());
        }
        let Some(mut map) = snapshot
            .map_frame
            .filter(|map| map.match_id == snapshot.match_id)
        else {
            self.log_status(
                identity,
                "feed",
                "live data available without map frame",
                now,
            );
            return Ok(());
        };
        if let Some(mut frame) = snapshot
            .announcement_frame
            .filter(|frame| frame.match_id == map.match_id && frame.game_time == map.game_time)
        {
            self.enrich_names(&mut frame, game.guild_id, &participants)
                .await;
            apply_map_display_names(&mut map, &frame);
        }
        self.log_status(identity, "feed", "fresh map data available", now);
        if crate::dota_spectator_recap::capture_map(
            self.path.clone(),
            game.guild_id,
            game.pending_match_id,
            map,
            now,
        )
        .await?
        {
            self.log_status(identity, "recap_capture", "capturing map frames", now);
        }
        Ok(())
    }
}
