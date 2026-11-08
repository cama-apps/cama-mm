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
// Guild-wide eligibility lists also include people who have never played. Only
// inherit the old account's frozen terms for matches they played or bet on.
const DEADLOCK_PLAYER_TERMS: &str = "match_id IN (
    SELECT match_id FROM deadlock_participants WHERE discord_id=?1
    UNION SELECT m.match_id FROM deadlock_betting_markets m JOIN deadlock_wagers w
        ON w.market_id=m.market_id AND w.guild_id=m.guild_id WHERE w.discord_id=?1
)";

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
        // Frozen bet-settlement rates are keyed by bettor.
        (
            "app_kv",
            "value",
            "key GLOB 'bet-settlement-rates:*'",
            "0",
            &["#"],
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
            DEADLOCK_PLAYER_TERMS,
            "status IN ('economic_setup','gathering','running','recorded')",
            &[],
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
            DEADLOCK_PLAYER_TERMS,
            "status IN ('open','closed')",
            &[],
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
            let row_id: i64 = row.get(0)?;
            let raw: String = row.get(1)?;
            let mut value: Value = serde_json::from_str(&raw).map_err(|_| {
                TransferError::Conflict(format!(
                    "Cannot read stored player data in {table}.{column}."
                ))
            })?;
            let mut changed = false;
            for path in paths {
                changed |= ids.move_at(&mut value, path).map_err(|()| {
                    TransferError::Conflict(format!(
                        "The replacement account already has stored player activity in {table}.{column} ({path}, row {row_id})."
                    ))
                })?;
            }
            if matches!(column, "economy_terms_json" | "terms_json") {
                changed |= ids.inherit_tax_eligibility(&mut value);
            } else if paths == PENDING
                && let Some(policy) = value.get_mut("recording_policy")
            {
                let owned = changed || (table == "app_kv" && has_archived_wager(tx, row_id, old)?);
                if owned {
                    changed |= ids.inherit_tax_eligibility(policy);
                }
            }
            if changed {
                if row.get::<_, bool>(2)? {
                    return Err(TransferError::Conflict(format!(
                        "The old account has an active operation in {table}. Finish it before transferring."
                    )));
                }
                update.execute(params![value.to_string(), row_id])?;
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

fn has_archived_wager(tx: &Transaction<'_>, row_id: i64, old: i64) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_kv k JOIN bets b ON b.guild_id=k.guild_id
         WHERE k.rowid=?1 AND b.discord_id=?2 AND
         (k.key='match-finalization:'||b.match_id OR k.key='pending-abort:'||b.pending_match_id))",
        params![row_id, old],
        |row| row.get(0),
    )
}

struct AccountIds {
    old: i64,
    new: i64,
    old_text: String,
    new_text: String,
}

impl AccountIds {
    fn inherit_tax_eligibility(&self, policy: &mut Value) -> bool {
        let mut changed = false;
        for field in ["low_priority_taxable_ids", "vanity_taxable_ids"] {
            if let Some(entries) = policy.get_mut(field).and_then(Value::as_array_mut) {
                let mut inherited = false;
                entries.retain_mut(|id| {
                    if id.as_i64() == Some(self.new) || id.as_str() == Some(&self.new_text) {
                        changed = true;
                        return false;
                    }
                    if id.as_i64() == Some(self.old) || id.as_str() == Some(&self.old_text) {
                        changed = true;
                        if inherited {
                            return false;
                        }
                        inherited = true;
                        *id = if id.is_string() {
                            Value::from(self.new_text.clone())
                        } else {
                            Value::from(self.new)
                        };
                    }
                    true
                });
            }
        }
        changed
    }

    // Err means the destination already owns this identity field.
    fn move_at(&self, value: &mut Value, path: &str) -> Result<bool, ()> {
        if path.is_empty() {
            if value.as_i64() == Some(self.new) || value.as_str() == Some(&self.new_text) {
                return Err(());
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
                    return Err(());
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
    fn guild_tax_snapshots_are_not_player_activity() {
        let db = fast_migrated_database();
        let mut conn = crate::open_runtime_connection(db.path()).unwrap();
        let unrelated =
            json!({"radiant_team_ids":[25],"recording_policy":{"vanity_taxable_ids":[OLD,NEW,25]}});
        conn.execute(
            "INSERT INTO pending_matches(guild_id,payload) VALUES(1,?1)",
            [unrelated.to_string()],
        )
        .unwrap();
        kv(&conn, "match-finalization:2", &unrelated.to_string());
        let prior = json!({"radiant_team_ids":[OLD],"recording_policy":{"vanity_taxable_ids":[NEW,25],"low_priority_taxable_ids":[OLD,NEW,26]}});
        kv(&conn, "match-finalization:3", &prior.to_string());
        let tx = conn.transaction().unwrap();
        transfer(&tx, OLD, NEW).unwrap();
        tx.commit().unwrap();
        assert_eq!(payload(&conn, "match-finalization:2"), unrelated);
        let updated = payload(&conn, "match-finalization:3");
        assert_eq!(updated["radiant_team_ids"], json!([NEW]));
        assert_eq!(
            updated["recording_policy"]["vanity_taxable_ids"],
            json!([25])
        );
        assert_eq!(
            updated["recording_policy"]["low_priority_taxable_ids"],
            json!([NEW, 26])
        );
    }
    #[test]
    fn bettors_inherit_their_frozen_terms_only_for_their_own_matches() {
        let db = fast_migrated_database();
        let mut conn = crate::open_runtime_connection(db.path()).unwrap();
        let terms =
            json!({"vanity_taxable_ids":[NEW], "low_priority_taxable_ids":[OLD.to_string(),NEW]});
        for (id, status) in [(1, "settled"), (2, "running")] {
            conn.execute("INSERT INTO deadlock_matches(match_id,guild_id,format,status,roster_json,created_by,created_at,updated_at,economy_terms_json)
                VALUES(?1,1,'standard',?2,'{}',42,0,0,?3)", params![id,status,terms.to_string()]).unwrap();
        }
        conn.execute("INSERT INTO deadlock_betting_markets(market_id,guild_id,match_id,format,roster_json,terms_json,deadline,status,created_at)
            VALUES('m',1,1,'standard','{}',?1,0,'settled',0)",[terms.to_string()]).unwrap();
        conn.execute("INSERT INTO deadlock_wagers(market_id,guild_id,discord_id,side,amount,leverage,effective_stake,request_key,kind,created_at)
            VALUES('m',1,?1,1,1,1,1,'w','manual',0)",[OLD]).unwrap();
        conn.execute("INSERT INTO bets(guild_id,match_id,discord_id,team_bet_on,amount,bet_time) VALUES(1,7,?1,'radiant',1,0)",[OLD]).unwrap();
        kv(
            &conn,
            "match-finalization:7",
            &json!({"recording_policy":terms}).to_string(),
        );
        let tx = conn.transaction().unwrap();
        transfer(&tx, OLD, NEW).unwrap();
        tx.commit().unwrap();
        let expected =
            json!({"vanity_taxable_ids":[], "low_priority_taxable_ids":[NEW.to_string()]});
        assert_eq!(
            payload(&conn, "match-finalization:7")["recording_policy"],
            expected
        );
        for (sql, want) in [
            ("SELECT terms_json FROM deadlock_betting_markets", &expected),
            (
                "SELECT economy_terms_json FROM deadlock_matches WHERE match_id=1",
                &expected,
            ),
            (
                "SELECT economy_terms_json FROM deadlock_matches WHERE match_id=2",
                &terms,
            ),
        ] {
            let raw: String = conn.query_row(sql, [], |r| r.get(0)).unwrap();
            assert_eq!(&serde_json::from_str::<Value>(&raw).unwrap(), want);
        }
    }

    #[test]
    fn moves_frozen_bet_settlement_rates_to_the_replacement_account() {
        let db = fast_migrated_database();
        let mut conn = Connection::open(db.path()).unwrap();
        let rates = json!({
            (OLD.to_string()): {"bankruptcy":0.15,"vanity_tax":0.0,"low_priority_tax":0.0},
            "37": {"bankruptcy":0.0,"vanity_tax":0.1,"low_priority_tax":0.0}
        });
        kv(&conn, "bet-settlement-rates:9", &rates.to_string());
        let tx = conn.transaction().unwrap();
        transfer(&tx, OLD, NEW).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            payload(&conn, "bet-settlement-rates:9"),
            json!({
                (NEW.to_string()): {"bankruptcy":0.15,"vanity_tax":0.0,"low_priority_tax":0.0},
                "37": {"bankruptcy":0.0,"vanity_tax":0.1,"low_priority_tax":0.0}
            })
        );
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
                .contains("app_kv.value (record_submissions.#")
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
