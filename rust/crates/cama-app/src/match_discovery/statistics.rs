//! Presence-aware coordinator statistics and OpenDota-compatible projection.

use super::*;
use cama_domain::role_derivation::FARM_PRIORITY_MINUTE;
use serde_json::{Map, Value};

fn value_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().and_then(truncate_finite_f64_to_i64))
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn field_i64(object: &Map<String, Value>, name: &str) -> Option<i64> {
    object.get(name).and_then(value_i64)
}

fn value_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn field_f64(object: &Map<String, Value>, name: &str) -> Option<f64> {
    object.get(name).and_then(value_f64)
}

fn field_bool(object: &Map<String, Value>, name: &str) -> Option<bool> {
    object.get(name).and_then(|value| {
        value
            .as_bool()
            .or_else(|| value_i64(value).map(|value| value != 0))
    })
}

fn truncate_finite_f64_to_i64(value: f64) -> Option<i64> {
    // `i64::MAX as f64` rounds up to 2^63, so the upper bound must be
    // exclusive. i64::MIN is exactly representable as f64.
    const I64_EXCLUSIVE_UPPER_BOUND: f64 = 9_223_372_036_854_775_808.0;

    (value.is_finite() && value >= i64::MIN as f64 && value < I64_EXCLUSIVE_UPPER_BOUND)
        .then(|| value.trunc() as i64)
}

pub fn project_match_details(
    value: Value,
    requested_match_id: i64,
) -> Option<OpenDotaMatchDetails> {
    let object = value.as_object()?;
    let raw_payload = serde_json::to_string(&value).ok();
    let players = object
        .get("players")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |players| {
            players.iter().filter_map(project_match_player).collect()
        });
    Some(OpenDotaMatchDetails {
        match_id: ValveMatchId(field_i64(object, "match_id").unwrap_or(requested_match_id)),
        duration_seconds: field_i64(object, "duration").unwrap_or(0),
        radiant_win: field_bool(object, "radiant_win").unwrap_or(false),
        radiant_score: field_i64(object, "radiant_score").unwrap_or(0),
        dire_score: field_i64(object, "dire_score").unwrap_or(0),
        game_mode: field_i64(object, "game_mode").unwrap_or(0),
        radiant_captain: captain_account_id(object.get("radiant_captain")),
        dire_captain: captain_account_id(object.get("dire_captain")),
        comeback: field_i64(object, "comeback"),
        throw_amount: field_i64(object, "throw"),
        raw_payload,
        players,
    })
}

fn captain_account_id(value: Option<&Value>) -> Option<SteamId> {
    let id = match value? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.parse::<i64>().ok(),
        _ => None,
    }?;
    (id > 0 && id < i64::from(u32::MAX)).then_some(SteamId(id))
}

fn project_match_player(value: &Value) -> Option<OpenDotaPlayer> {
    let object = value.as_object()?;
    let purchase_keys = object
        .get("purchase_log")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("key").and_then(Value::as_str))
                .map(ToOwned::to_owned)
                .collect()
        });
    Some(OpenDotaPlayer {
        account_id: field_i64(object, "account_id").map(SteamId),
        player_slot: field_i64(object, "player_slot").and_then(|value| u16::try_from(value).ok()),
        stats: EnrichedParticipantStats {
            hero_id: field_i64(object, "hero_id").unwrap_or(0),
            kills: field_i64(object, "kills").unwrap_or(0),
            deaths: field_i64(object, "deaths").unwrap_or(0),
            assists: field_i64(object, "assists").unwrap_or(0),
            gpm: field_i64(object, "gold_per_min").unwrap_or(0),
            xpm: field_i64(object, "xp_per_min").unwrap_or(0),
            hero_damage: field_i64(object, "hero_damage").unwrap_or(0),
            tower_damage: field_i64(object, "tower_damage").unwrap_or(0),
            last_hits: field_i64(object, "last_hits").unwrap_or(0),
            denies: field_i64(object, "denies").unwrap_or(0),
            net_worth: field_i64(object, "net_worth")
                .or_else(|| field_i64(object, "total_gold"))
                .unwrap_or(0),
            hero_healing: field_i64(object, "hero_healing").unwrap_or(0),
            lane_role: field_i64(object, "lane_role"),
            lane_efficiency: field_i64(object, "lane_efficiency_pct"),
            gold_at_10: object
                .get("gold_t")
                .and_then(Value::as_array)
                .and_then(|series| series.get(FARM_PRIORITY_MINUTE))
                .and_then(Value::as_i64),
            last_hits_at_10: object
                .get("lh_t")
                .and_then(Value::as_array)
                .and_then(|series| series.get(FARM_PRIORITY_MINUTE))
                .and_then(Value::as_i64),
            towers_killed: field_i64(object, "towers_killed"),
            roshans_killed: field_i64(object, "roshans_killed"),
            teamfight_participation: field_f64(object, "teamfight_participation"),
            obs_placed: field_i64(object, "obs_placed"),
            sen_placed: field_i64(object, "sen_placed"),
            camps_stacked: field_i64(object, "camps_stacked"),
            rune_pickups: field_i64(object, "rune_pickups"),
            firstblood_claimed: field_bool(object, "firstblood_claimed").map(i64::from),
            stuns: field_f64(object, "stuns"),
        },
        fantasy: FantasyStats {
            kills: field_f64(object, "kills").unwrap_or(0.0),
            deaths: object.get("deaths").and_then(value_f64),
            assists: field_f64(object, "assists").unwrap_or(0.0),
            last_hits: field_f64(object, "last_hits").unwrap_or(0.0),
            denies: field_f64(object, "denies").unwrap_or(0.0),
            gold_per_min: field_f64(object, "gold_per_min").unwrap_or(0.0),
            xp_per_min: field_f64(object, "xp_per_min").unwrap_or(0.0),
            towers_killed: field_f64(object, "towers_killed").unwrap_or(0.0),
            roshans_killed: field_f64(object, "roshans_killed").unwrap_or(0.0),
            teamfight_participation: field_f64(object, "teamfight_participation").unwrap_or(0.0),
            obs_placed: field_f64(object, "obs_placed").unwrap_or(0.0),
            sen_placed: field_f64(object, "sen_placed").unwrap_or(0.0),
            camps_stacked: field_f64(object, "camps_stacked").unwrap_or(0.0),
            rune_pickups: field_f64(object, "rune_pickups").unwrap_or(0.0),
            firstblood_claimed: field_bool(object, "firstblood_claimed").unwrap_or(false),
            stuns: field_f64(object, "stuns").unwrap_or(0.0),
            hero_healing: field_f64(object, "hero_healing").unwrap_or(0.0),
        },
        wrapped: WrappedPlayerTelemetry {
            actions_per_min: field_i64(object, "actions_per_min"),
            courier_kills: field_i64(object, "courier_kills"),
            pings: field_i64(object, "pings"),
            lane_role: field_i64(object, "lane_role"),
            purchase_keys,
        },
    })
}

const GC_MARKER: &str = "_cama_gc_statistics";
const BASIC_FIELDS: &[&str] = &[
    "hero_id",
    "kills",
    "deaths",
    "assists",
    "gold_per_min",
    "xp_per_min",
    "hero_damage",
    "tower_damage",
    "last_hits",
    "denies",
    "net_worth",
    "hero_healing",
];
const FANTASY_FIELDS: &[&str] = &[
    "kills",
    "deaths",
    "assists",
    "last_hits",
    "denies",
    "gold_per_min",
    "xp_per_min",
    "towers_killed",
    "roshans_killed",
    "teamfight_participation",
    "obs_placed",
    "sen_placed",
    "camps_stacked",
    "rune_pickups",
    "firstblood_claimed",
    "stuns",
    "hero_healing",
];

fn available(value: &Value) -> bool {
    !value.is_null() && !value.as_array().is_some_and(Vec::is_empty)
}

fn overlay(base: &mut Value, incoming: &Value) {
    let (Some(base), Some(incoming)) = (base.as_object_mut(), incoming.as_object()) else {
        return;
    };
    for (key, value) in incoming {
        if key == "players" {
            let Some(players) = value.as_array() else {
                continue;
            };
            let mut merged = base
                .remove(key)
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            for player in players {
                let Some(account) = player
                    .get("account_id")
                    .and_then(Value::as_i64)
                    .filter(|id| *id > 0)
                else {
                    continue;
                };
                if let Some(previous) = merged
                    .iter_mut()
                    .find(|p| p.get("account_id").and_then(Value::as_i64) == Some(account))
                {
                    overlay(previous, player);
                } else {
                    merged.push(player.clone());
                }
            }
            base.insert(key.clone(), Value::Array(merged));
        } else if available(value) {
            base.insert(key.clone(), value.clone());
        }
    }
}

pub(super) fn merged_details(
    previous: Option<&str>,
    incoming: Option<OpenDotaMatchDetails>,
    gc: &str,
    match_id: i64,
) -> Result<OpenDotaMatchDetails, String> {
    let gc: Value = serde_json::from_str(gc)
        .map_err(|e| format!("Invalid saved coordinator statistics: {e}"))?;
    if gc.get("match_id").and_then(Value::as_i64) != Some(match_id) {
        return Err("Saved coordinator statistics belong to a different Valve match.".into());
    }
    if incoming
        .as_ref()
        .is_some_and(|details| details.match_id.0 != match_id)
    {
        return Err("API statistics belong to a different Valve match.".into());
    }
    let mut merged = previous
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .filter(|value| value.get("match_id").and_then(Value::as_i64) == Some(match_id))
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(raw) = incoming.and_then(|details| details.raw_payload)
        && let Ok(value) = serde_json::from_str::<Value>(&raw)
    {
        overlay(&mut merged, &value);
    }
    overlay(&mut merged, &gc);
    let accounts: BTreeSet<_> = gc
        .get("players")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.get("account_id").and_then(Value::as_i64))
        .collect();
    if let Some(players) = merged.get_mut("players").and_then(Value::as_array_mut) {
        players.retain(|p| {
            p.get("account_id")
                .and_then(Value::as_i64)
                .is_some_and(|account| accounts.contains(&account))
        });
    }
    merged[GC_MARKER] = Value::Bool(true);
    project_match_details(merged, match_id).ok_or_else(|| "Invalid merged match statistics.".into())
}

/// Missing fields and missing real time series stay eligible for API backfill.
#[must_use]
pub fn statistics_need_api(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return true;
    };
    let Some(players) = value.get("players").and_then(Value::as_array) else {
        return true;
    };
    (value.get(GC_MARKER).and_then(Value::as_bool) == Some(true)
        && players.iter().any(|player| {
            FANTASY_FIELDS
                .iter()
                .any(|key| player.get(*key).is_none_or(|value| !available(value)))
        }))
        || players.len() != 10
        || players.iter().any(|player| {
            BASIC_FIELDS
                .iter()
                .any(|key| player.get(*key).is_none_or(|value| !available(value)))
        })
        || ["radiant_gold_adv", "radiant_xp_adv"].iter().any(|key| {
            value
                .get(*key)
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
        })
}

pub(super) fn presence(
    details: &OpenDotaMatchDetails,
    account: SteamId,
) -> Option<BTreeSet<String>> {
    let value: Value = serde_json::from_str(details.raw_payload.as_deref()?).ok()?;
    if value.get(GC_MARKER).and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let player = value
        .get("players")?
        .as_array()?
        .iter()
        .find(|player| player.get("account_id").and_then(Value::as_i64) == Some(account.0))?;
    Some(
        player
            .as_object()?
            .iter()
            .filter(|(_, value)| available(value))
            .map(|(key, _)| key.clone())
            .collect(),
    )
}

pub(super) fn fantasy_complete(presence: Option<&BTreeSet<String>>) -> bool {
    presence.is_none_or(|fields| FANTASY_FIELDS.iter().all(|key| fields.contains(*key)))
}
