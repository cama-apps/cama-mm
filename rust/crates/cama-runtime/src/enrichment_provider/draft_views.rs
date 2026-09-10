use super::*;

use cama_app::dedicated_lobby_channel::GuildId;
use cama_app::match_discovery::{MatchDiscoveryReadPort, ValveMatchId};
use cama_db::match_draft::{
    DraftBackfillCandidate, DraftHistoryPage, DraftMatchEntry, DrafterLeaderboardPage, DrafterStats,
};

impl EnrichmentHandler {
    pub(super) async fn handle_draft_backfill(
        &self,
        context: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(guild_id) = self.require_guild(&context, &responder).await? else {
            return Ok(());
        };
        if !self.require_admin(&context, &responder).await? || !defer(&responder, true).await {
            return Ok(());
        }
        if boolean_option(options, "status").unwrap_or(false) {
            let message = draft_backfill_status(&self.draft_backfill_status, guild_id)
                .unwrap_or_else(|| "No draft backfill has run for this server since startup. Run `/enrich drafts` to start or resume.".to_owned());
            return responder
                .followup(InteractionResponse::message(message).ephemeral())
                .await
                .map_err(response_error);
        }
        if self
            .draft_backfill_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            let message = draft_backfill_status(&self.draft_backfill_status, guild_id)
                .map(|status| format!("A draft backfill is already running.\n{status}"))
                .unwrap_or_else(|| {
                    "A draft backfill is already running. Please try again after it finishes."
                        .to_owned()
                });
            return responder
                .followup(InteractionResponse::message(message).ephemeral())
                .await
                .map_err(response_error);
        }
        let starting = "Backfilling all recorded drafts in the background. Check `/enrich drafts status:true` for progress. If the bot restarts, rerun `/enrich drafts` to resume from saved results.";
        set_draft_backfill_status(&self.draft_backfill_status, guild_id, starting.to_owned());
        if let Err(error) = responder
            .followup(InteractionResponse::message(starting).ephemeral())
            .await
        {
            self.draft_backfill_running.store(false, Ordering::Release);
            return Err(response_error(error));
        }
        let drafts = self.drafts.clone();
        let matches = self.matches.clone();
        let steam = self.steam.clone();
        let opendota = Arc::clone(&self.opendota);
        let analysis = Arc::clone(&self.draft_analysis);
        let running = Arc::clone(&self.draft_backfill_running);
        let status = Arc::clone(&self.draft_backfill_status);
        tokio::spawn(async move {
            let _guard = ParsedRefreshRunGuard(running.as_ref());
            let mut counts = BackfillCounts::default();
            let mut cursor = None;
            let final_message = loop {
                let page_repo = drafts.clone();
                let page = match run_blocking(move || {
                    page_repo.backfill_candidates(guild_id, 10, cursor)
                })
                .await
                {
                    Ok(page) => page,
                    Err(error) => {
                        warn!(guild_id, %error, "draft backfill could not load next page");
                        break format!(
                            "**Draft backfill stopped**\nCould not load the next page. Saved results are preserved; rerun `/enrich drafts` to resume.\n{}",
                            counts.message(false)
                        );
                    }
                };
                if page.is_empty() {
                    break counts.message(true);
                }
                for candidate in page {
                    if counts.processed > 0 {
                        tokio::time::sleep(Duration::from_millis(1100)).await;
                    }
                    let result = loop {
                        let candidate = candidate.clone();
                        let matches = matches.clone();
                        let drafts = drafts.clone();
                        let steam = steam.clone();
                        let opendota = Arc::clone(&opendota);
                        let analysis = Arc::clone(&analysis);
                        let result = run_blocking(move || {
                            backfill_one(
                                &candidate,
                                guild_id,
                                &matches,
                                &drafts,
                                &steam,
                                &opendota,
                                analysis.as_ref(),
                            )
                        })
                        .await;
                        if let Ok(outcome) = &result
                            && let Some(delay) = outcome.retry_after
                        {
                            set_draft_backfill_status(
                                &status,
                                guild_id,
                                format!(
                                    "{}\nWaiting for provider quota; the current match will be retried.",
                                    counts.message(false),
                                ),
                            );
                            tokio::time::sleep(delay.max(Duration::from_millis(100))).await;
                            continue;
                        }
                        break result;
                    };
                    cursor = Some(candidate.match_id);
                    counts.processed += 1;
                    match result {
                        Ok(outcome) => {
                            counts.estimated += usize::from(outcome.estimated);
                            counts.captains_unknown += usize::from(outcome.captains_unknown);
                            counts.errors += usize::from(outcome.had_error);
                        }
                        Err(error) => {
                            counts.errors += 1;
                            warn!(guild_id,match_id=cursor,%error,"draft backfill entry failed");
                        }
                    }
                    set_draft_backfill_status(&status, guild_id, counts.message(false));
                }
                if let Err(error) = responder
                    .edit_original(InteractionResponse::message(counts.message(false)).ephemeral())
                    .await
                {
                    warn!(guild_id, %error, "draft backfill progress message unavailable; processing continues");
                }
            };
            set_draft_backfill_status(&status, guild_id, final_message.clone());
            if let Err(error) = responder
                .edit_original(InteractionResponse::message(final_message).ephemeral())
                .await
            {
                warn!(guild_id, %error, "draft backfill completed without updating original message");
            }
        });
        Ok(())
    }

    pub(super) async fn handle_draft_stats(
        &self,
        context: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(guild_id) = self.require_guild(&context, &responder).await? else {
            return Ok(());
        };
        if !defer(&responder, true).await {
            return Ok(());
        }
        let route = DraftPageRoute {
            guild_id,
            view: options.iter().find_map(|option| match (option.name.as_str(), &option.value) {
                ("view", InteractionValue::String(value)) => Some(match value.as_str() {
                    "best_drafters" => 1, "worst_drafters" => 2, _ => 0,
                }),
                _ => None,
            }).unwrap_or(0),
            user_id: user_option(options, "user")?.map(|user| user.id),
            by_imbalance: options.iter().any(|option| {
                option.name == "sort"
                    && matches!(&option.value, InteractionValue::String(value) if value == "imbalance")
            }),
            page: usize::try_from(integer_option(options, "page").unwrap_or(1).max(1))
                .unwrap_or(1),
            limit: integer_option(options, "limit").unwrap_or(5).clamp(1, 10) as usize,
        };
        let response = self.render_draft_history(route).await?.ephemeral();
        responder.followup(response).await.map_err(response_error)
    }

    pub(super) async fn handle_draft_page(
        &self,
        custom_id: &str,
        guild_id: Option<i64>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(route) = DraftPageRoute::parse(custom_id) else {
            return respond_initial(
                &responder,
                InteractionResponse::message(
                    "This draft history control is invalid. Run `/matches drafts` again.",
                )
                .ephemeral(),
            )
            .await;
        };
        if guild_id != Some(route.guild_id) {
            return respond_initial(
                &responder,
                InteractionResponse::message("This draft history belongs to another server.")
                    .ephemeral(),
            )
            .await;
        }
        if !defer(&responder, true).await {
            return Ok(());
        }
        let response = self.render_draft_history(route).await?;
        responder.update(response).await.map_err(response_error)
    }

    async fn render_draft_history(
        &self,
        mut route: DraftPageRoute,
    ) -> Result<InteractionResponse, InteractionHandlerError> {
        let drafts = self.drafts.clone();
        run_blocking(move || {
            if route.view != 0 {
                let mut leaderboard = drafts
                    .drafter_leaderboard(
                        route.guild_id,
                        route.user_id,
                        route.view == 2,
                        route.page,
                        route.limit,
                    )
                    .map_err(|error| error.to_string())?;
                let pages = leaderboard.total.div_ceil(route.limit).max(1);
                if route.page > pages {
                    route.page = pages;
                    leaderboard = drafts
                        .drafter_leaderboard(
                            route.guild_id,
                            route.user_id,
                            route.view == 2,
                            route.page,
                            route.limit,
                        )
                        .map_err(|error| error.to_string())?;
                }
                return Ok(drafter_leaderboard_response(&route, &leaderboard));
            }
            let mut history = drafts
                .draft_history(
                    route.guild_id,
                    route.user_id,
                    route.by_imbalance,
                    route.page,
                    route.limit,
                )
                .map_err(|error| error.to_string())?;
            let pages = history.total.div_ceil(route.limit).max(1);
            if route.page > pages {
                route.page = pages;
                history = drafts
                    .draft_history(
                        route.guild_id,
                        route.user_id,
                        route.by_imbalance,
                        route.page,
                        route.limit,
                    )
                    .map_err(|error| error.to_string())?;
            }
            let stats = route
                .user_id
                .map(|id| drafts.drafter_stats(route.guild_id, id))
                .transpose()
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(draft_history_response(&route, &history, stats.as_ref()))
        })
        .await
        .map_err(InteractionHandlerError::from)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DraftPageRoute {
    guild_id: i64,
    user_id: Option<i64>,
    by_imbalance: bool,
    view: u8,
    page: usize,
    limit: usize,
}

impl DraftPageRoute {
    fn parse(custom_id: &str) -> Option<Self> {
        let parts = custom_id.split(':').collect::<Vec<_>>();
        let ["enrichment", "drafts", guild, user, mode, page, limit, view] = parts.as_slice()
        else {
            return None;
        };
        if [guild, user, mode, page, limit, view]
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return None;
        }
        let guild_id = guild.parse::<i64>().ok().filter(|value| *value > 0)?;
        let user_id = user.parse::<i64>().ok()?;
        let by_imbalance = match *mode {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        let view = view.parse::<u8>().ok().filter(|value| *value <= 2)?;
        let page = page.parse::<usize>().ok().filter(|value| *value > 0)?;
        let limit = limit
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=10).contains(value))?;
        i64::try_from((page - 1).checked_mul(limit)?).ok()?;
        Some(Self {
            guild_id,
            user_id: (user_id != 0).then_some(user_id),
            by_imbalance,
            view,
            page,
            limit,
        })
    }

    fn custom_id(&self, page: usize) -> String {
        format!(
            "enrichment:drafts:{}:{}:{}:{page}:{}:{}",
            self.guild_id,
            self.user_id.unwrap_or(0),
            usize::from(self.by_imbalance),
            self.limit,
            self.view
        )
    }
}

fn draft_history_response(
    route: &DraftPageRoute,
    history: &DraftHistoryPage,
    stats: Option<&DrafterStats>,
) -> InteractionResponse {
    let pages = history.total.div_ceil(route.limit).max(1);
    let summary = route
        .user_id
        .zip(stats)
        .map(|(id, stats)| format!("<@{id}> — {}\n\n", drafter_summary(stats)))
        .unwrap_or_default();
    let empty = if history.entries.is_empty() {
        "No recorded draft estimates found.\n\n"
    } else {
        ""
    };
    let mut embed = EmbedModel {
        title: Some(
            if route.by_imbalance {
                "Most uneven drafts"
            } else {
                "Draft history"
            }
            .to_owned(),
        ),
        description: Some(format!(
            "{summary}{empty}Page {} of {pages} · {} matches\nDraft estimates by [Batru](https://batru.gg). 48–52% is a split.",
            route.page, history.total
        )),
        ..EmbedModel::default()
    };
    for entry in &history.entries {
        embed.add_field(
            format!("Match #{}", entry.match_id),
            draft_line(entry),
            false,
        );
    }
    let mut embed = embed_model(embed);
    embed.color = Some(DISCORD_BLUE);
    let response = InteractionResponse::message("").embed(embed);
    with_draft_page_buttons(response, route, pages)
}

fn with_draft_page_buttons(
    response: InteractionResponse,
    route: &DraftPageRoute,
    pages: usize,
) -> InteractionResponse {
    if pages <= 1 {
        return response;
    }
    response.action_row(InteractionActionRow::buttons(vec![
        InteractionButton::new(
            route.custom_id(route.page.saturating_sub(1).max(1)),
            "< Previous",
        )
        .disabled(route.page == 1),
        InteractionButton::new(
            route.custom_id(route.page.saturating_add(1).min(pages)),
            "Next >",
        )
        .disabled(route.page >= pages),
    ]))
}

fn drafter_leaderboard_response(
    route: &DraftPageRoute,
    leaderboard: &DrafterLeaderboardPage,
) -> InteractionResponse {
    let pages = leaderboard.total.div_ceil(route.limit).max(1);
    let mut embed = EmbedModel {
        title: Some(
            if route.view == 2 {
                "Least successful drafters"
            } else {
                "Most successful drafters"
            }
            .to_owned(),
        ),
        description: Some(format!(
            "Page {} of {pages} · {} drafters\nRanked by draft wins / (wins + losses). Splits and unknown estimates are excluded; drafters need at least one decisive draft.\nEstimates by [Batru](https://batru.gg).",
            route.page, leaderboard.total
        )),
        ..EmbedModel::default()
    };
    if leaderboard.entries.is_empty() {
        embed.add_field(
            "No ranked drafters",
            "No decisive draft estimates found for this selection.",
            false,
        );
    }
    for (index, entry) in leaderboard.entries.iter().enumerate() {
        let rank = (route.page - 1)
            .saturating_mul(route.limit)
            .saturating_add(index + 1);
        embed.add_field(
            format!("#{rank}"),
            format!(
                "<@{}>\n{}\nDecisive drafts: **{}**",
                entry.discord_id,
                drafter_summary(&entry.stats),
                entry.stats.drafts_won + entry.stats.drafts_lost
            ),
            false,
        );
    }
    let mut embed = embed_model(embed);
    embed.color = Some(DISCORD_BLUE);
    with_draft_page_buttons(InteractionResponse::message("").embed(embed), route, pages)
}

type DraftBackfillStatus = std::sync::Mutex<BTreeMap<i64, String>>;

fn draft_backfill_status(status: &DraftBackfillStatus, guild_id: i64) -> Option<String> {
    status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&guild_id)
        .cloned()
}

fn set_draft_backfill_status(status: &DraftBackfillStatus, guild_id: i64, message: String) {
    status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(guild_id, message);
}

struct BackfillOutcome {
    estimated: bool,
    captains_unknown: bool,
    had_error: bool,
    retry_after: Option<Duration>,
}

fn backfill_one(
    candidate: &DraftBackfillCandidate,
    guild_id: i64,
    matches: &MatchRepository,
    drafts: &MatchDraftRepository,
    steam: &OpenDotaPlayerRepository,
    opendota: &OpenDotaRuntimeServices,
    analysis: &dyn DraftEnrichmentPort,
) -> Result<BackfillOutcome, String> {
    let stored = candidate
        .enrichment_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .filter(|value| {
            value.get("match_id").and_then(serde_json::Value::as_i64)
                == Some(candidate.valve_match_id)
        })
        .and_then(|value| {
            cama_app::opendota_http::project_match_details(value, candidate.valve_match_id)
        })
        .filter(|details| {
            details.match_id.0 == candidate.valve_match_id && draft_roster_complete(details)
        });
    let needs_fetch = stored.as_ref().is_none_or(|details| {
        details.game_mode == 2
            && (details.radiant_captain.is_none() || details.dire_captain.is_none())
    });
    let mut had_error = false;
    let details = if needs_fetch {
        match opendota
            .enrichment_api()
            .match_details(ValveMatchId(candidate.valve_match_id))
        {
            Ok(Some(details))
                if details.match_id.0 == candidate.valve_match_id
                    && draft_roster_complete(&details) =>
            {
                details
            }
            result => {
                had_error = true;
                warn!(
                    match_id = candidate.match_id,
                    available = result.is_ok(),
                    "OpenDota draft backfill details unavailable or invalid"
                );
                stored.ok_or_else(|| "no complete draft roster available".to_owned())?
            }
        }
    } else {
        stored.ok_or_else(|| "draft roster unavailable".to_owned())?
    };
    let participants = MatchDiscoveryReadPort::get_match_participants(
        matches,
        InternalMatchId(candidate.match_id),
        GuildId(guild_id),
    )
    .map_err(|error| error.to_string())?;
    let ids = participants
        .iter()
        .map(|participant| participant.discord_id)
        .collect::<Vec<_>>();
    let links = PlayerSteamIdPort::get_steam_ids_bulk(steam, &ids, GuildId(guild_id))
        .map_err(|error| error.to_string())?;
    // The durable match link identifies this historical game. Current Steam
    // links resolve captain mentions; account changes cannot invalidate heroes.
    let mut retry_after = None;
    if let Err(error) = analysis.enrich_draft(candidate.match_id, guild_id, &details, &links) {
        had_error = true;
        retry_after = analysis.retry_after();
        warn!(match_id=candidate.match_id,%error,"draft backfill partially complete");
    }
    let saved = drafts
        .get(candidate.match_id, guild_id)
        .map_err(|error| error.to_string())?;
    let estimated = saved
        .as_ref()
        .is_some_and(|row| row.radiant_win_probability_bps.is_some());
    let captains_unknown = details.game_mode == 2
        && saved.as_ref().is_none_or(|row| {
            row.radiant_drafter_steam_id.is_none()
                || row.dire_drafter_steam_id.is_none()
                || row.radiant_drafter_discord_id.is_none()
                || row.dire_drafter_discord_id.is_none()
        });
    Ok(BackfillOutcome {
        estimated,
        captains_unknown,
        had_error,
        retry_after,
    })
}

fn draft_roster_complete(details: &OpenDotaMatchDetails) -> bool {
    let slots = details
        .players
        .iter()
        .filter_map(|player| player.player_slot)
        .collect::<BTreeSet<_>>();
    let heroes = details
        .players
        .iter()
        .map(|player| player.stats.hero_id)
        .collect::<BTreeSet<_>>();
    details.players.len() == 10
        && slots == BTreeSet::from([0, 1, 2, 3, 4, 128, 129, 130, 131, 132])
        && heroes.len() == 10
        && heroes.iter().all(|hero| *hero > 0)
}

#[derive(Default)]
struct BackfillCounts {
    processed: usize,
    estimated: usize,
    captains_unknown: usize,
    errors: usize,
}

impl BackfillCounts {
    fn message(&self, finished: bool) -> String {
        format!(
            "**Draft backfill {}**\nProcessed: {} · With estimates: {} · Captains unknown: {} · Errors/partial failures: {}\nEstimates by [Batru](https://batru.gg).",
            if finished { "complete" } else { "in progress" },
            self.processed,
            self.estimated,
            self.captains_unknown,
            self.errors
        )
    }
}

fn drafter_summary(stats: &DrafterStats) -> String {
    let decisive = stats.drafts_won + stats.drafts_lost;
    let rate = if decisive == 0 {
        "N/A".to_owned()
    } else {
        format!("{:.1}%", 100.0 * stats.drafts_won as f64 / decisive as f64)
    };
    format!(
        "Draft W/L/S: **{}/{}/{}** · Win rate: **{rate}** (splits excluded) · Unknown: {}",
        stats.drafts_won, stats.drafts_lost, stats.drafts_split, stats.drafts_unknown
    )
}

fn draft_line(entry: &DraftMatchEntry) -> String {
    let outcome = match entry.winning_team {
        Some(1) => "Radiant won",
        Some(2) => "Dire won",
        _ => "Unknown",
    };
    let analysis = entry.analysis.as_ref();
    let probability = analysis
        .and_then(|row| row.radiant_win_probability_bps)
        .filter(|value| (0..=10_000).contains(value));
    let estimate = probability
        .map(|rad| {
            let favored = match cama_domain::draft_analysis::draft_winner(rad) {
                Some(1) => "Radiant favored",
                Some(2) => "Dire favored",
                _ => "Split",
            };
            format!(
                "Draft: **{favored}**\nRadiant **{}.{:02}%** · Dire **{}.{:02}%**",
                rad / 100,
                rad % 100,
                (10000 - rad) / 100,
                (10000 - rad) % 100
            )
        })
        .unwrap_or_else(|| "Draft: **Unknown** · Draft estimate unavailable".to_owned());
    let drafter = |id: Option<i64>| {
        id.filter(|id| *id > 0)
            .map_or_else(|| "Unknown".to_owned(), |id| format!("<@{id}>"))
    };
    let links = entry.valve_match_id.filter(|id| *id > 0).map(|id| format!(
        "[OpenDota](https://www.opendota.com/matches/{id}) · [Dotabuff](https://www.dotabuff.com/matches/{id}) · [STRATZ](https://stratz.com/matches/{id})"
    )).unwrap_or_else(|| format!("`/matches view match_id:{}`", entry.match_id));
    format!(
        "Game result: **{outcome}**\n{estimate}\nDrafters: Radiant {} · Dire {}\n{links}",
        drafter(analysis.and_then(|row| row.radiant_drafter_discord_id)),
        drafter(analysis.and_then(|row| row.dire_drafter_discord_id))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_summary_excludes_splits_and_unknowns_from_win_rate() {
        let summary = drafter_summary(&DrafterStats {
            drafts_won: 3,
            drafts_lost: 1,
            drafts_split: 5,
            drafts_unknown: 2,
        });
        assert!(summary.contains("75.0%"));
        assert!(summary.contains("3/1/5"));
        assert!(summary.contains("Unknown: 2"));
        assert!(drafter_summary(&DrafterStats::default()).contains("N/A"));
    }

    #[test]
    fn draft_stats_lines_distinguish_draft_and_game_outcomes() {
        let line = draft_line(&DraftMatchEntry {
            match_id: 7,
            valve_match_id: Some(100),
            winning_team: Some(2),
            analysis: Some(MatchDraftAnalysis {
                radiant_win_probability_bps: Some(4896),
                draft_winner: Some(0),
                radiant_drafter_discord_id: Some(42),
                ..Default::default()
            }),
        });
        assert!(line.contains("48.96%"));
        assert!(line.contains("51.04%"));
        assert!(line.contains("Split"));
        assert!(line.contains("<@42>"));
        assert!(line.contains("Dire Unknown"));
        assert!(line.contains("Game result: **Dire won**"));
        for domain in [
            "opendota.com/matches/100",
            "dotabuff.com/matches/100",
            "stratz.com/matches/100",
        ] {
            assert!(line.contains(domain));
        }
        assert!(
            BackfillCounts::default()
                .message(true)
                .contains("https://batru.gg")
        );
    }

    #[test]
    fn draft_history_unlinked_unknown_has_no_fabricated_probability() {
        let line = draft_line(&DraftMatchEntry {
            match_id: 7,
            valve_match_id: None,
            winning_team: Some(1),
            analysis: None,
        });
        assert!(line.contains("Game result: **Radiant won**"));
        assert!(line.contains("Draft estimate unavailable"));
        assert!(line.contains("Drafters: Radiant Unknown · Dire Unknown"));
        assert!(line.contains("`/matches view match_id:7`"));
        assert!(!line.contains('%'));
        assert!(!line.contains("Split"));
    }

    #[test]
    fn draft_page_routes_validate_every_field_and_round_trip_filters() {
        for route in [
            DraftPageRoute {
                guild_id: 42,
                user_id: None,
                by_imbalance: false,
                view: 0,
                page: 1,
                limit: 5,
            },
            DraftPageRoute {
                guild_id: 42,
                user_id: Some(100),
                by_imbalance: true,
                view: 0,
                page: 3,
                limit: 10,
            },
        ] {
            assert_eq!(
                DraftPageRoute::parse(&route.custom_id(route.page)),
                Some(route)
            );
        }
        for invalid in [
            "enrichment:drafts:0:0:0:1:5",
            "enrichment:drafts:42:-1:0:1:5",
            "enrichment:drafts:42:0:2:1:5",
            "enrichment:drafts:42:0:0:0:5",
            "enrichment:drafts:42:0:0:1:11",
            "enrichment:drafts:42:0:0:1:0",
            "enrichment:drafts:42:0:0:1:5:extra",
            "enrichment:match:42:0:0:1:5",
            "enrichment:drafts:42:0:0:18446744073709551615:10",
        ] {
            assert_eq!(DraftPageRoute::parse(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn draft_page_buttons_preserve_filters_and_disable_boundaries() {
        let route = DraftPageRoute {
            guild_id: 42,
            user_id: Some(100),
            by_imbalance: true,
            view: 0,
            page: 1,
            limit: 5,
        };
        let history = DraftHistoryPage {
            entries: Vec::new(),
            total: 11,
        };
        let response = draft_history_response(&route, &history, None);
        let buttons = &response.components[0].buttons;
        assert!(buttons[0].disabled);
        assert!(!buttons[1].disabled);
        assert_eq!(buttons[1].custom_id, "enrichment:drafts:42:100:1:2:5:0");
        let route = DraftPageRoute { page: 3, ..route };
        let response = draft_history_response(&route, &history, None);
        assert_eq!(
            response.components[0].buttons[0].custom_id,
            "enrichment:drafts:42:100:1:2:5:0"
        );
        assert!(!response.components[0].buttons[0].disabled);
        assert!(response.components[0].buttons[1].disabled);
        assert!(
            response.embeds[0]
                .description
                .as_deref()
                .unwrap()
                .contains("Page 3 of 3")
        );
    }

    #[test]
    fn ten_detailed_drafts_stay_within_discord_embed_limits() {
        let entries = (1..=10)
            .map(|id| DraftMatchEntry {
                match_id: i64::MAX - id,
                valve_match_id: Some(i64::MAX - id),
                winning_team: Some(1),
                analysis: Some(MatchDraftAnalysis {
                    radiant_win_probability_bps: Some(6166),
                    draft_winner: Some(1),
                    radiant_drafter_discord_id: Some(i64::MAX),
                    dire_drafter_discord_id: Some(i64::MAX),
                    ..Default::default()
                }),
            })
            .collect();
        let route = DraftPageRoute {
            guild_id: 42,
            user_id: None,
            by_imbalance: false,
            view: 0,
            page: 1,
            limit: 10,
        };
        let response =
            draft_history_response(&route, &DraftHistoryPage { entries, total: 10 }, None);
        let embed = &response.embeds[0];
        let total = embed.title.as_ref().map_or(0, String::len)
            + embed.description.as_ref().map_or(0, String::len)
            + embed
                .fields
                .iter()
                .map(|field| field.name.len() + field.value.len())
                .sum::<usize>();
        assert!(total < 6000, "{total}");
        assert!(embed.fields.iter().all(|field| field.value.len() <= 1024));
    }
}
