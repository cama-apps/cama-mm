use super::TransferError;
use rusqlite::{Transaction, params};
use serde_json::Value;

// Every path describes player identity in a specific persisted format. "*" selects
// array entries, "#" selects the keys of a player-indexed map, and "0" selects
// the first member of a tuple. Free text, opaque request keys, Steam IDs and
// the signed contents of completed draft financial plans retain their provenance.
const PENDING: &[&str] = &[
    "radiant_team_ids.*",
    "dire_team_ids.*",
    "excluded_player_ids.*",
    "excluded_conditional_player_ids.*",
    "record_submissions.#",
    "full_exclusion_increment_ids.*",
    "half_exclusion_increment_ids.*",
    "record_streaming_ids.*",
    "shuffle_streaming_bonus_ids.*",
    "recording_policy.low_priority_taxable_ids.*",
    "recording_policy.vanity_taxable_ids.*",
    "_rust_match_easter_streaks.win_streak_records.*.discord_id",
    "blind_bets_result.bets.*.discord_id",
    "blind_bets_result.skipped.*.discord_id",
    "blind_bets_result.investment_bets.bets.*.investor_id",
    "blind_bets_result.investment_bets.bets.*.target_id",
    "blind_bets_result.investment_bets.bets.*.discord_id",
    "blind_bets_result.investment_bets.skipped.*.discord_id",
    "blind_bets_result.investment_bets.skipped.*.investor_id",
    "blind_bets_result.investment_bets.skipped.*.target_id",
    "blind_bets_result.spectator_bets.bets.*.discord_id",
    "blind_bets_result.spectator_bets.skipped.*.discord_id",
];
const DRAFT: &[&str] = &[
    "state.player_pool_ids.*",
    "state.player_pool_data.#",
    "state.excluded_player_ids.*",
    "state.full_exclusion_increment_ids.*",
    "state.excluded_conditional_player_ids.*",
    "state.half_exclusion_increment_ids.*",
    "state.captain1_id",
    "state.captain2_id",
    "state.radiant_captain_id",
    "state.dire_captain_id",
    "state.coinflip_winner_id",
    "state.current_round_first_captain_id",
    "state.radiant_player_ids.*",
    "state.dire_player_ids.*",
    "state.side_preferences.#",
];
const TEAMS: &[&str] = &["team1.*.discord_id", "team2.*.discord_id"];
const TAXABLE: &[&str] = &["low_priority_taxable_ids.*", "vanity_taxable_ids.*"];

pub(super) fn transfer(tx: &Transaction<'_>, old: i64, new: i64) -> Result<(), TransferError> {
    let ids = AccountIds {
        old,
        new,
        old_text: old.to_string(),
        new_text: new.to_string(),
    };
    // table, column, row filter, expression indicating a live in-memory owner, ID fields
    type Column = (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static [&'static str],
    );
    let columns: &[Column] = &[
        ("matches", "team1_players", "1", "0", &["*"]),
        ("matches", "team2_players", "1", "0", &["*"]),
        ("matches", "jc_changes", "1", "0", &["#"]),
        (
            "lobby_state",
            "players",
            "1",
            "lower(status)='open'",
            &["*"],
        ),
        (
            "lobby_state",
            "conditional_players",
            "1",
            "lower(status)='open'",
            &["*"],
        ),
        (
            "lobby_state",
            "player_join_times",
            "1",
            "lower(status)='open'",
            &["#"],
        ),
        ("pending_matches", "payload", "1", "1", PENDING),
        (
            "app_kv",
            "value",
            "key GLOB 'match-finalization:*' OR key GLOB 'pending-abort:*'",
            "0",
            PENDING,
        ),
        (
            "app_kv",
            "value",
            "key='draft:state'",
            "CASE WHEN json_valid(value) THEN coalesce(json_extract(value,'$.active'),1) OR coalesce(json_extract(value,'$.finalizing'),0) ELSE 1 END",
            DRAFT,
        ),
        (
            "app_kv",
            "value",
            "key GLOB 'readycheck:pruned_notice:*'",
            "1",
            &["player_ids.*"],
        ),
        (
            "app_kv",
            "value",
            "key GLOB 'blame_luke:operation:*' AND CASE WHEN json_valid(value) THEN json_extract(value,'$.status')='Charged' ELSE 1 END",
            "1",
            &["user_id"],
        ),
        (
            "dota_sessions",
            "payload",
            "1",
            "phase NOT IN ('recorded','cancelled','failed')",
            &["roster.*.discord_id"],
        ),
        (
            "dota_spectators",
            "payload",
            "1",
            "1",
            &[
                "participants.*",
                "viewers.*",
                "joined_viewers.*",
                "subscription_batch.viewers.*",
            ],
        ),
        (
            "deadlock_matches",
            "roster_json",
            "1",
            "status IN ('economic_setup','gathering','running','recorded')",
            TEAMS,
        ),
        (
            "deadlock_matches",
            "economy_terms_json",
            "1",
            "status IN ('economic_setup','gathering','running','recorded')",
            TAXABLE,
        ),
        (
            "deadlock_betting_markets",
            "roster_json",
            "1",
            "status IN ('open','closed')",
            TEAMS,
        ),
        (
            "deadlock_betting_markets",
            "terms_json",
            "1",
            "status IN ('open','closed')",
            TAXABLE,
        ),
        ("deadlock_economic_receipts", "detail_json", "kind IN ('settlement','refund')", "0",
            &["gross_payouts.#", "deductions.#", "refunds.#"]),
        ("deadlock_economic_receipts", "detail_json", "kind='blood_pact_pending' AND NOT EXISTS (
             SELECT 1 FROM deadlock_economic_receipts done WHERE done.market_id=deadlock_economic_receipts.market_id
             AND done.guild_id=deadlock_economic_receipts.guild_id AND done.discord_id=deadlock_economic_receipts.discord_id
             AND done.kind='blood_pact_complete')", "1", &["discord_id"]),
        ("disburse_history", "recipients", "1", "0", &["*.0"]),
        ("predictions", "resolution_votes", "1", "0", &["#"]),
        (
            "manashop_buffs",
            "data",
            "1",
            "0",
            &["caster_id", "ally_id", "protected_user_ids.*"],
        ),
        (
            "mafia_games",
            "resolution_summary",
            "1",
            "0",
            &[
                "lynched_id",
                "mvp_id",
                "bookie_id",
                "winning_ids.*",
                "bankruptcy_penalties.#",
                "vanity_taxes.#",
                "low_priority_taxes.#",
                "vote_breakdown.#",
                "vote_detail.*.*",
            ],
        ),
        (
            "hostile_loss_events",
            "metadata",
            "json_valid(metadata)",
            "0",
            &["skimmer_id"],
        ),
        ("tunnels", "cheer_data", "1", "0", &["*.cheerer_id"]),
        (
            "dig_actions",
            "detail",
            "json_valid(detail)",
            "0",
            &["target_id", "attacker_id", "skimmer_id"],
        ),
        (
            "pet_refund_windows",
            "announcement_payload",
            "1",
            "0",
            &["payouts.*.discord_id"],
        ),
    ];
    // Unfinished financial journals can resume from any intermediate stage. Opaque
    // receipt keys and extension fields can themselves contain the player ID.
    // Wait for completion rather than altering their integrity-checked inputs.
    let mut journals = tx.prepare(
        "SELECT plan_json,progress_json FROM draft_finalization_jobs WHERE stage!='complete'",
    )?;
    let mut rows = journals.query([])?;
    while let Some(row) = rows.next()? {
        for index in 0..2 {
            if row
                .get::<_, String>(index)?
                .split(|c: char| !c.is_ascii_digit())
                .any(|token| token == ids.old_text || token == ids.new_text)
            {
                return Err(TransferError::Conflict(
                    "Wait for the pending draft finalization to finish.".into(),
                ));
            }
        }
    }
    for &(table, column, filter, live, paths) in columns {
        let select = format!(
            "SELECT rowid,{column},coalesce(({live}),0) FROM {table}
             WHERE ({filter}) AND (instr({column},?1)>0 OR instr({column},?2)>0) ORDER BY rowid"
        );
        let update = format!("UPDATE {table} SET {column}=?1 WHERE rowid=?2");
        let mut statement = tx.prepare(&select)?;
        let mut update = tx.prepare(&update)?;
        let mut rows = statement.query(params![ids.old_text, ids.new_text])?;
        while let Some(row) = rows.next()? {
            let raw: String = row.get(1)?;
            let mut value: Value = serde_json::from_str(&raw).map_err(|_| {
                TransferError::Conflict(format!(
                    "Cannot read stored player data in {table}.{column}."
                ))
            })?;
            let mut changed = false;
            for path in paths {
                changed |= ids.move_at(&mut value, path)?;
            }
            if changed {
                if row.get::<_, bool>(2)? {
                    return Err(TransferError::Conflict(format!(
                        "The old account has an active operation in {table}. Finish it before transferring."
                    )));
                }
                update.execute(params![value.to_string(), row.get::<_, i64>(0)?])?;
            }
        }
    }
    // These short-lived prompts are bound to a Discord interaction. Removing
    // them prevents either account from applying a stale registration.
    tx.execute(
        "DELETE FROM app_kv WHERE key GLOB 'registration:mmr:*'
         AND substr(value,1,instr(value,':')-1) IN (?1,?2)",
        [&ids.old_text, &ids.new_text],
    )?;
    Ok(())
}

struct AccountIds {
    old: i64,
    new: i64,
    old_text: String,
    new_text: String,
}

impl AccountIds {
    fn destination_conflict() -> TransferError {
        TransferError::Conflict(
            "The replacement account already has stored player activity.".into(),
        )
    }

    fn move_at(&self, value: &mut Value, path: &str) -> Result<bool, TransferError> {
        if path.is_empty() {
            if value.as_i64() == Some(self.new) || value.as_str() == Some(&self.new_text) {
                return Err(Self::destination_conflict());
            }
            if value.as_i64() == Some(self.old) {
                *value = Value::from(self.new);
                return Ok(true);
            }
            if value.as_str() == Some(&self.old_text) {
                *value = Value::from(self.new_text.clone());
                return Ok(true);
            }
            return Ok(false);
        }
        let (part, rest) = path.split_once('.').unwrap_or((path, ""));
        match part {
            "*" => {
                let mut changed = false;
                if let Some(items) = value.as_array_mut() {
                    for item in items {
                        changed |= self.move_at(item, rest)?;
                    }
                }
                Ok(changed)
            }
            "#" => {
                let Some(map) = value.as_object_mut() else {
                    return Ok(false);
                };
                if map.contains_key(&self.new_text) {
                    return Err(Self::destination_conflict());
                }
                if let Some(entry) = map.remove(&self.old_text) {
                    map.insert(self.new_text.clone(), entry);
                    return Ok(true);
                }
                Ok(false)
            }
            "0" => value
                .get_mut(0)
                .map_or(Ok(false), |item| self.move_at(item, rest)),
            field => value
                .get_mut(field)
                .map_or(Ok(false), |item| self.move_at(item, rest)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fast_migrated_database;
    use rusqlite::Connection;
    use serde_json::json;

    const OLD: i64 = 123_456_789_123_456_789;
    const NEW: i64 = 234_567_891_234_567_891;

    fn kv(connection: &Connection, key: &str, payload: &str) {
        connection
            .execute("INSERT INTO app_kv VALUES(1,?1,?2)", params![key, payload])
            .unwrap();
    }

    fn payload(connection: &Connection, key: &str) -> Value {
        let raw: String = connection
            .query_row("SELECT value FROM app_kv WHERE key=?1", [key], |r| r.get(0))
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn moves_archived_rosters_votes_and_rewards_without_rewriting_unrelated_numbers_or_text() {
        let db = fast_migrated_database();
        let mut conn = Connection::open(db.path()).unwrap();
        let archive = json!({
            "radiant_team_ids": [OLD.to_string(), 37],
            "record_submissions": {(OLD.to_string()): {"result":"radiant"}},
            "blind_bets_result": {"investment_bets":{"bets":[{"target_id":OLD, "amount":OLD}], "skipped":[{"investor_id":OLD, "target_id":37}]}},
            "effective_deal_ids":[OLD], "steam_id": OLD, "shuffle_channel_id": NEW,
            "note":format!("old user {OLD}")
        });
        kv(&conn, "match-finalization:5", &archive.to_string());
        kv(&conn, "pending-abort:6", &archive.to_string());
        kv(&conn, "blame_luke:operation:bad", "unrelated bad json");
        kv(&conn, "registration:mmr:1", &format!("{OLD}:12:0:200000:0"));
        kv(&conn, "registration:mmr:2", &format!("{NEW}:14:0:200000:0"));
        kv(
            &conn,
            "registration:mmr:3",
            &format!("999:{OLD}:0:200000:0"),
        );
        let tx = conn.transaction().unwrap();
        transfer(&tx, OLD, NEW).unwrap();
        tx.commit().unwrap();
        for key in ["match-finalization:5", "pending-abort:6"] {
            let mut expected = archive.clone();
            expected["radiant_team_ids"][0] = json!(NEW.to_string());
            expected["record_submissions"] = json!({(NEW.to_string()):{"result":"radiant"}});
            expected["blind_bets_result"]["investment_bets"]["bets"][0]["target_id"] = json!(NEW);
            expected["blind_bets_result"]["investment_bets"]["skipped"][0]["investor_id"] =
                json!(NEW);
            assert_eq!(payload(&conn, key), expected);
        }
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM app_kv WHERE key GLOB 'registration:mmr:*'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn refuses_active_players_and_destination_activity_and_preserves_hashes_in_completed_jobs() {
        let db = fast_migrated_database();
        let mut conn = Connection::open(db.path()).unwrap();
        let archive = json!({"radiant_team_ids":[OLD]});
        kv(&conn, "match-finalization:2", &archive.to_string());
        let active = json!({"state":{"player_pool_ids":[OLD]},"active":true,"finalizing":false});
        kv(&conn, "draft:state", &active.to_string());
        assert!(
            transfer(&conn.transaction().unwrap(), OLD, NEW)
                .unwrap_err()
                .to_string()
                .contains("active")
        );
        assert_eq!(payload(&conn, "match-finalization:2"), archive);
        conn.execute("UPDATE app_kv SET value=json_set(value,'$.active',json('false')) WHERE key='draft:state'", []).unwrap();
        let plan = json!({"opaque":{"effect_key":format!("draft:4:5:settle:{OLD}")}}).to_string();
        conn.execute("INSERT INTO draft_finalization_jobs(completion_key,guild_id,session_id,pending_match_id,stage,plan_json) VALUES('test',1,2,3,'linked',?1)",[&plan]).unwrap();
        assert!(
            transfer(&conn.transaction().unwrap(), OLD, NEW)
                .unwrap_err()
                .to_string()
                .contains("finalization")
        );
        conn.execute("UPDATE draft_finalization_jobs SET stage='complete'", [])
            .unwrap();
        kv(
            &conn,
            "pending-abort:3",
            &json!({"record_submissions":{(NEW.to_string()):{}}}).to_string(),
        );
        assert!(
            transfer(&conn.transaction().unwrap(), OLD, NEW)
                .unwrap_err()
                .to_string()
                .contains("replacement")
        );
        conn.execute("DELETE FROM app_kv WHERE key='pending-abort:3'", [])
            .unwrap();
        let tx = conn.transaction().unwrap();
        transfer(&tx, OLD, NEW).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            payload(&conn, "draft:state")["state"]["player_pool_ids"],
            json!([NEW])
        );
        assert_eq!(
            conn.query_row("SELECT plan_json FROM draft_finalization_jobs", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            plan
        );
    }
}
