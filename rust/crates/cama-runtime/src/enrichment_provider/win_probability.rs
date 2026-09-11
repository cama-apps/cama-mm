//! Load a persisted, match-bound graph. Clicking never contacts Steam or Valve.
use super::*;
use cama_app::drawing::draw_win_probability_graph;

fn graph_values(raw: &str, valve_id: i64) -> Option<Vec<f64>> {
    let payload: serde_json::Value = serde_json::from_str(raw).ok()?;
    if payload.get("match_id")?.as_i64()? != valve_id {
        return None;
    }
    let graph = payload.get("_cama_win_probability")?;
    if graph.get("match_id")?.as_i64()? != valve_id
        || graph.get("axis")?.as_str()? != "sample_index"
        || graph.get("unit")?.as_str()? != "percent"
        || graph.get("side")?.as_str()? != "radiant"
    {
        return None;
    }
    let values = graph.get("values")?.as_array()?;
    if !(2..=4096).contains(&values.len()) {
        return None;
    }
    values
        .iter()
        .map(|value| {
            value
                .as_f64()
                .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
        })
        .collect()
}

pub(super) fn load_graph(
    matches: &MatchRepository,
    guild_id: i64,
    match_id: i64,
) -> Result<Option<Vec<f64>>, String> {
    let Some(valve_id) = matches
        .get_match(match_id, Some(guild_id))
        .map_err(|e| e.to_string())?
        .and_then(|row| row.valve_match_id)
    else {
        return Ok(None);
    };
    // Prefer the immutable GC source; enriched copies may arrive later.
    for raw in [
        matches
            .gc_statistics(match_id, Some(guild_id))
            .map_err(|e| e.to_string())?,
        matches
            .raw_enrichment_data(match_id, Some(guild_id))
            .map_err(|e| e.to_string())?,
    ]
    .into_iter()
    .flatten()
    {
        if let Some(values) = graph_values(&raw, valve_id) {
            return Ok(Some(values));
        }
    }
    Ok(None)
}

impl EnrichmentHandler {
    pub(super) async fn show_win_probability(
        &self,
        route: MatchViewRoute,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let matches = self.matches.clone();
        let image = run_blocking(move || {
            Ok::<_, String>(
                load_graph(&matches, route.guild_id, route.match_id)?
                    .and_then(|values| draw_win_probability_graph(&values, route.match_id))
                    .map(|image| image.into_inner()),
            )
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        let Some(image) = image else {
            return respond_initial(
                &responder,
                InteractionResponse::message(
                    "The postgame win-probability graph is not available for this match yet.",
                )
                .ephemeral(),
            )
            .await;
        };
        responder.update(InteractionResponse::message(String::new())
            .embed(InteractionEmbed::default().color(DISCORD_DARKER)
                .image("attachment://win-probability.png")
                .footer("Valve postgame history · ordered samples, not game-clock timestamps"))
            .attachment(InteractionAttachment::bytes("win-probability.png",image))
            .action_row(match_view_buttons(route.guild_id,route.match_id,route.expires_at,2)))
            .await.map_err(response_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chart_requires_matching_identity_axis_and_valid_percentages() {
        let mut payload = serde_json::json!({"match_id":123,"_cama_win_probability":{
            "match_id":123,"axis":"sample_index","unit":"percent","side":"radiant","values":[0,50,100]
        }});
        assert_eq!(
            graph_values(&payload.to_string(), 123),
            Some(vec![0.0, 50.0, 100.0])
        );
        assert!(graph_values(&payload.to_string(), 124).is_none());
        payload["_cama_win_probability"]["match_id"] = serde_json::json!(124);
        assert!(graph_values(&payload.to_string(), 123).is_none());
        payload["_cama_win_probability"]["match_id"] = serde_json::json!(123);
        payload["_cama_win_probability"]["axis"] = serde_json::json!("minutes");
        assert!(graph_values(&payload.to_string(), 123).is_none());
        payload["_cama_win_probability"]["axis"] = serde_json::json!("sample_index");
        payload["_cama_win_probability"]["values"] = serde_json::json!([0, 101]);
        assert!(graph_values(&payload.to_string(), 123).is_none());
    }
}
