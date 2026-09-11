//! Creation-time routing for one dedicated Dota host. All decisions share the
//! pending insertion writer transaction; a busy game is permanently manual.
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const POLICY_KEY: &str = "dota_host_routing";
pub const ACCOUNT_KEY: &str = "dota_host_account_key";
pub const FALLBACK_REASON: &str = "dota_hosting_fallback_reason";
const REQUESTED_HOSTING: &str = "dota_host_routing_requested_hosting";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DotaHostRouting {
    pub account_key: String,
    pub guild_ids: Vec<i64>,
}

fn invalid(message: impl ToString) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(message.to_string())
}

fn table_exists(connection: &Connection, table: &str) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |r| r.get(0),
    )
}

/// Publish the enabled host before accepting Discord interactions. Unclaimed
/// historical games and withdrawn reservations retain normal manual workflows.
pub fn configure(
    connection: &mut Connection,
    routing: Option<DotaHostRouting>,
) -> rusqlite::Result<()> {
    if let Some(policy) = &routing
        && (policy
            .account_key
            .parse::<u32>()
            .ok()
            .is_none_or(|id| id == 0)
            || policy.guild_ids.is_empty()
            || policy.guild_ids.iter().any(|id| *id <= 0))
    {
        return Err(invalid(
            "Dota routing requires a positive account and server IDs",
        ));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute("INSERT INTO app_kv(guild_id,key,value) VALUES(0,?1,?2) ON CONFLICT(guild_id,key) DO UPDATE SET value=excluded.value", params![POLICY_KEY, serde_json::to_string(&routing).map_err(invalid)?])?;
    let rows = {
        let mut statement =
            transaction.prepare("SELECT guild_id,pending_match_id,payload FROM pending_matches")?;
        statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (guild, pending, raw) in rows {
        let active: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM dota_sessions WHERE guild_id=?1 AND pending_match_id=?2 AND phase NOT IN ('recorded','cancelled','failed'))", params![guild,pending], |r| r.get(0))?;
        if active {
            continue;
        }
        let mut payload: Value = serde_json::from_str(&raw).map_err(invalid)?;
        let retained = routing.as_ref().is_some_and(|policy| {
            policy.guild_ids.contains(&guild)
                && payload.get(ACCOUNT_KEY).and_then(Value::as_str)
                    == Some(policy.account_key.as_str())
        });
        if !retained && payload.get(ACCOUNT_KEY).is_some() {
            // Tombstone outside the hashed Draft document. Re-enabling the
            // same config must never resurrect a withdrawn reservation.
            transaction.execute("INSERT INTO app_kv(guild_id,key,value) VALUES(0,?1,'true') ON CONFLICT(guild_id,key) DO NOTHING", [format!("dota_host_withdrawn:{guild}:{pending}")])?;
        }
        // Linked Draft plans hash their initial payload; materialize withdrawal
        // only when terminal finalization owns its last payload mutation.
        if table_exists(&transaction, "draft_finalization_jobs")?
            && transaction.query_row("SELECT EXISTS(SELECT 1 FROM draft_finalization_jobs WHERE guild_id=?1 AND pending_match_id=?2 AND stage!='complete')", params![guild,pending], |r|r.get::<_,bool>(0))? { continue; }
        if retained && !withdrawn(&transaction, guild, pending)? {
            continue;
        }
        let reason = if payload.get(ACCOUNT_KEY).is_some() {
            "hosting_unavailable"
        } else {
            "legacy_pending"
        };
        if payload.get(FALLBACK_REASON).is_none() {
            manual(&mut payload, reason)?;
        }
        clear_reservation(&mut payload)?;
        transaction.execute("UPDATE pending_matches SET payload=?1,updated_at=CURRENT_TIMESTAMP WHERE guild_id=?2 AND pending_match_id=?3", params![payload.to_string(),guild,pending])?;
    }
    transaction.commit()
}

fn clear_reservation(payload: &mut Value) -> rusqlite::Result<()> {
    let object = payload
        .as_object_mut()
        .ok_or_else(|| invalid("pending payload must be an object"))?;
    for key in [
        ACCOUNT_KEY,
        "dota_hosted_betting",
        "dota_hosted_betting_observed_at",
    ] {
        object.remove(key);
    }
    Ok(())
}

fn manual(payload: &mut Value, reason: &str) -> rusqlite::Result<()> {
    clear_reservation(payload)?;
    let object = payload
        .as_object_mut()
        .ok_or_else(|| invalid("pending payload must be an object"))?;
    let hosting = object
        .entry("dota_hosting".to_owned())
        .or_insert_with(|| json!({}));
    hosting
        .as_object_mut()
        .ok_or_else(|| invalid("Dota hosting settings must be an object"))?
        .insert("hosting".into(), json!("manual"));
    object.insert(FALLBACK_REASON.into(), json!(reason));
    Ok(())
}

/// Must run inside the writer transaction which inserts this pending match.
/// Missing policy is supported only for old isolated repository fixtures.
pub fn route_pending(
    connection: &Connection,
    guild: i64,
    payload: &mut Value,
) -> rusqlite::Result<()> {
    if !table_exists(connection, "app_kv")? {
        return Ok(());
    }
    let raw: Option<String> = connection
        .query_row(
            "SELECT value FROM app_kv WHERE guild_id=0 AND key=?1",
            [POLICY_KEY],
            |r| r.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(());
    };
    let policy: Option<DotaHostRouting> = serde_json::from_str(&raw).map_err(invalid)?;
    let requested = payload.get("dota_hosting").cloned().unwrap_or(Value::Null);
    let defaults: Option<String> = connection
        .query_row(
            "SELECT dota_hosting_options FROM guild_config WHERE guild_id=?1",
            [guild],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let defaults: cama_domain::dota_hosting::DotaHostingOptions =
        serde_json::from_str(defaults.as_deref().unwrap_or("{}")).map_err(invalid)?;
    let explicit: cama_domain::dota_hosting::DotaHostingOptions =
        serde_json::from_value(if requested.is_null() {
            json!({})
        } else {
            requested.clone()
        })
        .map_err(invalid)?;
    let mut effective = defaults.merged(&explicit);
    effective.validate().map_err(invalid)?;
    let explicitly_manual =
        effective.hosting == Some(cama_domain::dota_hosting::HostingMode::Manual);
    effective.hosting = Some(if explicitly_manual {
        cama_domain::dota_hosting::HostingMode::Manual
    } else {
        cama_domain::dota_hosting::HostingMode::Bot
    });
    payload["dota_hosting"] = serde_json::to_value(effective).map_err(invalid)?;
    clear_reservation(payload)?;
    payload
        .as_object_mut()
        .ok_or_else(|| invalid("pending payload must be an object"))?
        .insert(REQUESTED_HOSTING.into(), requested.clone());
    if explicitly_manual {
        return manual(payload, "manual_requested");
    }
    let Some(policy) = policy else {
        return manual(payload, "hosting_disabled");
    };
    if !policy.guild_ids.contains(&guild) {
        return manual(payload, "hosting_unavailable");
    }
    let busy: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM dota_sessions WHERE account_key=?1 AND phase NOT IN ('recorded','cancelled','failed')) OR EXISTS(SELECT 1 FROM pending_matches p WHERE json_extract(p.payload,'$.dota_host_account_key')=?1 AND COALESCE(json_extract(p.payload,'$.dota_hosting.hosting'),'bot')!='manual' AND NOT EXISTS(SELECT 1 FROM app_kv k WHERE k.guild_id=0 AND k.key='dota_host_withdrawn:'||p.guild_id||':'||p.pending_match_id))", [&policy.account_key], |r|r.get(0))?;
    if busy {
        return manual(payload, "bot_busy");
    }
    let radiant = payload.get("radiant_team_ids").and_then(Value::as_array);
    let dire = payload.get("dire_team_ids").and_then(Value::as_array);
    let Some((radiant, dire)) = radiant
        .zip(dire)
        .filter(|(a, b)| a.len() == 5 && b.len() == 5)
    else {
        return manual(payload, "invalid_roster");
    };
    let players = radiant
        .iter()
        .chain(dire)
        .filter_map(Value::as_i64)
        .collect::<BTreeSet<_>>();
    if players.len() != 10 || players.iter().any(|id| *id <= 0) {
        return manual(payload, "invalid_roster");
    }
    let mut accounts = BTreeSet::new();
    for player in players {
        let primary: Option<i64> = connection.query_row("SELECT steam_id FROM player_steam_ids WHERE discord_id=?1 AND is_primary=1 ORDER BY added_at,id LIMIT 1",[player],|r|r.get(0)).optional()?;
        let steam = match primary {Some(id)=>Some(id),None=>connection.query_row("SELECT steam_id FROM players WHERE discord_id=?1 AND steam_id IS NOT NULL ORDER BY guild_id LIMIT 1",[player],|r|r.get(0)).optional()?};
        let Some(account) = steam.and_then(cama_domain::dota_lobby::account_id) else {
            return manual(payload, "missing_steam_links");
        };
        if account.to_string() == policy.account_key || !accounts.insert(account) {
            return manual(payload, "invalid_steam_links");
        }
    }
    let configured_league: Option<i64> = connection
        .query_row(
            "SELECT league_id FROM guild_config WHERE guild_id=?1",
            [guild],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let league = payload
        .get("dota_hosting")
        .and_then(|v| v.get("league_id"))
        .and_then(Value::as_i64)
        .or(configured_league);
    if league.is_none_or(|id| id <= 0 || u32::try_from(id).is_err()) {
        return manual(payload, "missing_league");
    }
    let object = payload.as_object_mut().expect("validated object");
    object.remove(FALLBACK_REASON);
    object.insert(ACCOUNT_KEY.into(), json!(policy.account_key));
    Ok(())
}

/// Reapply only derived routing fields on a Draft retry. Requested hosting and
/// every other original field remain part of the immutable payload comparison.
pub fn replay_routing(requested: &str, persisted: &str) -> rusqlite::Result<String> {
    let original_requested = requested;
    let original_persisted = persisted;
    let mut requested: Value = serde_json::from_str(requested).map_err(invalid)?;
    let persisted: Value = serde_json::from_str(persisted).map_err(invalid)?;
    if requested == persisted {
        return Ok(original_persisted.to_owned());
    }
    if let Some(original) = persisted.get(REQUESTED_HOSTING) {
        if requested.get("dota_hosting").unwrap_or(&Value::Null) != original {
            return Err(invalid("Draft hosting request changed during retry"));
        }
        if let Some(hosting) = persisted.get("dota_hosting") {
            requested["dota_hosting"] = hosting.clone();
        }
        for key in [ACCOUNT_KEY, FALLBACK_REASON, REQUESTED_HOSTING] {
            if let Some(value) = persisted.get(key) {
                requested[key] = value.clone();
            }
        }
        if persisted.get(FALLBACK_REASON).is_some() {
            if requested.get("dota_hosting").is_none() {
                requested["dota_hosting"] = json!({});
            }
            requested["dota_hosting"]["hosting"] = json!("manual");
        }
        return Ok(requested.to_string());
    }
    Ok(original_requested.to_owned())
}

pub fn withdrawn(connection: &Connection, guild: i64, pending: i64) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_kv WHERE guild_id=0 AND key=?1)",
        [format!("dota_host_withdrawn:{guild}:{pending}")],
        |r| r.get(0),
    )
}

pub fn apply_withdrawn(connection: &Connection, guild: i64, pending: i64) -> rusqlite::Result<()> {
    if !withdrawn(connection, guild, pending)? {
        return Ok(());
    }
    let raw: Option<String> = connection
        .query_row(
            "SELECT payload FROM pending_matches WHERE guild_id=?1 AND pending_match_id=?2",
            params![guild, pending],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(raw) = raw {
        let mut payload: Value = serde_json::from_str(&raw).map_err(invalid)?;
        manual(&mut payload, "hosting_unavailable")?;
        connection.execute(
            "UPDATE pending_matches SET payload=?1 WHERE guild_id=?2 AND pending_match_id=?3",
            params![payload.to_string(), guild, pending],
        )?;
    }
    Ok(())
}
