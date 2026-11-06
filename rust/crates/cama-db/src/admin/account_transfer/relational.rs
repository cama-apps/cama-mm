use rusqlite::{Transaction, params};

use super::TransferError;

// Names are part of the storage contract. A generic search for ID columns would
// also change Steam IDs, Discord channel IDs, and internal survey recipient IDs.
const PLAYER_REFERENCES: &[(&str, &[&str])] = &[
    ("autobet_investments", &["investor_id", "target_id"]),
    ("bankruptcy_state", &["discord_id"]),
    ("bet_settlement_taxes", &["discord_id"]),
    ("bets", &["discord_id", "investment_target_id"]),
    (
        "deadlock_betting_investments",
        &["investor_id", "target_id"],
    ),
    ("deadlock_betting_liquidity", &["discord_id"]),
    ("deadlock_economic_receipts", &["discord_id"]),
    ("deadlock_matches", &["created_by", "recorded_by"]),
    ("deadlock_participants", &["discord_id"]),
    ("deadlock_players", &["discord_id"]),
    ("deadlock_queue", &["discord_id"]),
    ("deadlock_rating_events", &["discord_id"]),
    ("deadlock_ratings", &["discord_id"]),
    ("deadlock_wagers", &["discord_id"]),
    ("dig_achievements", &["discord_id"]),
    ("dig_actions", &["actor_id", "target_id"]),
    ("dig_active_duels", &["discord_id", "echo_killer_id"]),
    ("dig_artifact_registry", &["first_finder_id"]),
    ("dig_artifacts", &["discord_id"]),
    ("dig_boss_echoes", &["killer_discord_id"]),
    ("dig_dm_memory", &["discord_id"]),
    ("dig_gear", &["discord_id"]),
    ("dig_inventory", &["discord_id"]),
    ("dig_personality", &["discord_id"]),
    ("dig_quests", &["discord_id"]),
    ("disburse_vote_history", &["discord_id"]),
    ("disburse_votes", &["discord_id"]),
    ("dota_spectator_subscriptions", &["user_id"]),
    ("double_or_nothing_spins", &["discord_id"]),
    (
        "duel_challenges",
        &[
            "challenger_id",
            "recipient_id",
            "winner_id",
            "resolution_actor_id",
        ],
    ),
    ("economy_ledger_context", &["actor_id"]),
    ("economy_ledger_entries", &["actor_id"]),
    ("hostile_loss_events", &["victim_id", "actor_id"]),
    ("loan_state", &["discord_id"]),
    ("lobby_state", &["created_by"]),
    ("lobby_suspensions", &["discord_id", "actor_id"]),
    (
        "lobby_target_subscriptions",
        &["target_id", "subscriber_id"],
    ),
    (
        "low_priority_state",
        &["discord_id", "set_by", "removed_by"],
    ),
    ("mafia_actions", &["actor_id", "target_id"]),
    ("mafia_bounties", &["target_id", "contributor_id"]),
    ("mafia_games", &["mvp_id"]),
    ("mafia_optout", &["discord_id"]),
    ("mafia_players", &["discord_id"]),
    ("mafia_signups", &["discord_id"]),
    ("mana_protection_events", &["victim_id"]),
    ("manashop_buffs", &["discord_id", "target_id"]),
    ("manashop_daily_uses", &["discord_id"]),
    ("manashop_purchases", &["discord_id"]),
    ("match_corrections", &["corrected_by"]),
    (
        "match_draft_analysis",
        &["radiant_drafter_discord_id", "dire_drafter_discord_id"],
    ),
    ("match_participants", &["discord_id"]),
    ("neon_events", &["discord_id"]),
    ("openskill_rating_events", &["discord_id", "actor_id"]),
    (
        "package_deal_purchases",
        &["buyer_discord_id", "partner_discord_id"],
    ),
    ("package_deals", &["buyer_discord_id", "partner_discord_id"]),
    ("pet_accessories", &["discord_id"]),
    (
        "pet_brawls",
        &["challenger_id", "recipient_id", "winner_id"],
    ),
    ("pet_diary_hatches", &["discord_id"]),
    ("pet_keepsakes", &["discord_id"]),
    ("pet_stable_state", &["discord_id"]),
    ("pet_supplies", &["discord_id"]),
    ("pets", &["discord_id"]),
    ("player_curfew_pending_changes", &["discord_id"]),
    ("player_curfew_windows", &["discord_id"]),
    ("player_mana", &["discord_id"]),
    ("player_pool_bets", &["discord_id"]),
    ("player_stakes", &["discord_id"]),
    ("player_steam_ids", &["discord_id"]),
    ("player_trivia_sessions", &["discord_id"]),
    ("players", &["discord_id"]),
    ("prediction_positions", &["discord_id"]),
    ("prediction_trades", &["discord_id"]),
    ("predictions", &["creator_id", "resolved_by"]),
    ("push_notification_targets", &["discord_id"]),
    ("rating_history", &["discord_id"]),
    ("recalibration_state", &["discord_id"]),
    ("referrals", &["referred_id", "referrer_id"]),
    ("reminder_preferences", &["discord_id"]),
    ("slow_drip_claims", &["discord_id"]),
    ("soft_avoids", &["avoider_discord_id", "avoided_discord_id"]),
    ("spectator_bets", &["discord_id"]),
    ("survey_recipients", &["discord_id"]),
    ("tax_fine_cooldowns", &["discord_id", "last_actor_id"]),
    ("tip_transactions", &["sender_id", "recipient_id"]),
    ("trivia_sessions", &["discord_id"]),
    ("tunnels", &["discord_id", "revenge_target"]),
    ("vanity_tax_enforcements", &["discord_id", "enforced_by"]),
    ("vanity_tax_exemptions", &["discord_id", "exempted_by"]),
    ("wheel_spins", &["discord_id"]),
    ("wrapped_enrichment_facts", &["discord_id"]),
    ("wrapped_generation", &["generated_by"]),
];

pub(super) fn transfer(tx: &Transaction<'_>, old: i64, new: i64) -> Result<(), TransferError> {
    // Archived display names describe the Discord account actually seen. The
    // replacement may already have one just from joining the server. Keep both.
    // Moderation and Deadlock audit records likewise retain the original attribution.
    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM survey_recipients
          WHERE discord_id=?1 AND (delivery_status='sending' OR attempt_count>receipt_checked_attempt))",
        [old],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(TransferError::Conflict(
            "A survey delivery to the old account is in progress. Retry after delivery settles.".into(),
        ));
    }

    tx.execute(
        "UPDATE push_notification_targets SET ntfy_topic=NULL WHERE discord_id=?1",
        [old],
    )?;
    // Keep answers and progress, but never deliver through a private channel
    // belonging to the compromised account. Requeue unfinished open surveys.
    tx.execute(
        "UPDATE survey_recipients SET dm_channel_id=NULL,dm_message_id=NULL,
             ui_channel_id=NULL,ui_message_id=NULL,
             delivery_status=CASE WHEN submitted_at IS NULL AND EXISTS (
                 SELECT 1 FROM surveys s WHERE s.survey_id=survey_recipients.survey_id
                     AND s.status='open') THEN 'pending' ELSE delivery_status END
          WHERE discord_id=?1",
        [old],
    )?;

    for &(table, columns) in PLAYER_REFERENCES {
        move_columns(tx, table, columns, "", old, new)?;
    }
    move_columns(
        tx,
        "economy_ledger_entries",
        &["account_id"],
        " AND account_type='player'",
        old,
        new,
    )?;
    move_columns(
        tx,
        "hostile_loss_events",
        &["recipient_id"],
        " AND destination='player'",
        old,
        new,
    )?;

    // related_id is also used for match IDs and other unrelated keyspaces.
    // Only the ledger types whose related object is a Discord player move.
    for table in ["economy_ledger_entries", "economy_ledger_context"] {
        move_columns(tx, table, &["related_id"],
            " AND related_type IN ('player','tax_fine','bankruptcy','bankruptcy_stipend','admin_adjustment','player_tip','player_debt','loan','referral',
                'pet_adopt','pet_upgrade','pet_sacrifice','pet_supplies','pet_rename','pet_salt_lick',
                'pet_trinket','mentor_bonus','help','sabotage','sabotage_steal','vendetta_reflect')",
            old, new)?;
    }

    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM player_pairings WHERE player1_id=?1 OR player2_id=?1)",
        [new],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(destination_conflict("player_pairings"));
    }
    // A pair is stored ascending, with wins_against relative to player1.
    // All right-hand sides of SQLite UPDATE see the original pair.
    tx.execute(
        "UPDATE player_pairings SET
             player1_id=MIN(?2,CASE WHEN player1_id=?1 THEN player2_id ELSE player1_id END),
             player2_id=MAX(?2,CASE WHEN player1_id=?1 THEN player2_id ELSE player1_id END),
             player1_wins_against=CASE WHEN
                 (player1_id=?1 AND ?2>player2_id) OR (player2_id=?1 AND ?2<player1_id)
                 THEN games_against-player1_wins_against ELSE player1_wins_against END
         WHERE player1_id=?1 OR player2_id=?1",
        params![old, new],
    )?;
    Ok(())
}

fn move_columns(
    tx: &Transaction<'_>,
    table: &str,
    columns: &[&str],
    filter: &str,
    old: i64,
    new: i64,
) -> Result<(), TransferError> {
    // Identifiers and the optional discriminator are fixed above. Both IDs are bound.
    let matches = columns
        .iter()
        .map(|col| format!("{col}=?1"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let condition = format!("({matches}){filter}");
    if tx.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {condition})"),
        [new],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(destination_conflict(table));
    }
    // All references change in one row update: a recorded winner, for example,
    // must still be one of the two participants for the table's CHECK to pass.
    let assignments = columns
        .iter()
        .map(|col| format!("{col}=CASE WHEN {col}=?1 THEN ?2 ELSE {col} END"))
        .collect::<Vec<_>>()
        .join(",");
    tx.execute(
        &format!("UPDATE {table} SET {assignments} WHERE {condition}"),
        params![old, new],
    )?;
    Ok(())
}

fn destination_conflict(table: &str) -> TransferError {
    TransferError::Conflict(format!(
        "The replacement account already has data in {table}."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{open_runtime_connection, test_support::fast_migrated_database};

    #[test]
    fn all_explicit_discord_columns_and_player_foreign_keys_have_a_transfer_policy() {
        let database = fast_migrated_database();
        let db = open_runtime_connection(database.path()).unwrap();
        let mut statement = db
            .prepare(
                "SELECT t.name,c.name FROM sqlite_schema t,pragma_table_info(t.name) c
             WHERE t.type='table' AND c.name LIKE '%discord_id'
             UNION
             SELECT t.name,f.[from] FROM sqlite_schema t,pragma_foreign_key_list(t.name) f
             WHERE t.type='table' AND f.[table]='players' AND f.[to]='discord_id'",
            )
            .unwrap();
        let refs = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap();
        for row in refs {
            let (table, column) = row.unwrap();
            if matches!(table.as_str(), "moderation_events" | "player_display_names") {
                continue;
            }
            assert!(
                PLAYER_REFERENCES
                    .iter()
                    .any(|&(t, cols)| t == table && cols.contains(&column.as_str())),
                "Add an account-transfer policy for {table}.{column}",
            );
        }
    }

    #[test]
    fn preserves_pairing_direction_completed_brawls_and_non_player_ids() {
        let database = fast_migrated_database();
        let mut db = open_runtime_connection(database.path()).unwrap();
        db.execute_batch(
            "INSERT INTO player_pairings(guild_id,player1_id,player2_id,games_against,player1_wins_against)
             VALUES(7,10,20,9,7),(7,5,10,5,1),(7,10,50,8,2);
             INSERT INTO pet_brawls(guild_id,channel_id,challenger_id,recipient_id,challenger_pet_id,
                 status,created_at,expires_at,winner_id,winner_pet_id)
             VALUES(7,10,10,21,10,'done',1,2,10,10);
             INSERT INTO economy_ledger_entries(guild_id,account_type,account_id,delta,balance_before,
                 balance_after,actor_id,related_type,related_id)
             VALUES(10,'nonprofit',10,1,0,1,10,'match','10'),
                   (7,'player',10,2,1,3,10,'player_tip','10');
             INSERT INTO player_display_names VALUES (10,7,'Old account',30),(40,7,'Replacement',40);",
        ).unwrap();
        let tx = db.transaction().unwrap();
        transfer(&tx, 10, 40).unwrap();
        tx.commit().unwrap();
        let mut pairs = db
            .prepare(
                "SELECT player1_id,player2_id,player1_wins_against
                                    FROM player_pairings ORDER BY player1_id",
            )
            .unwrap();
        let outcomes = pairs
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(outcomes, vec![(5, 40, 1), (20, 40, 2), (40, 50, 2)]);
        let correct: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pet_brawls WHERE challenger_id=40 AND winner_id=40
                 AND recipient_id=21 AND channel_id=10 AND winner_pet_id=10)
               AND EXISTS(SELECT 1 FROM economy_ledger_entries WHERE account_type='nonprofit'
                 AND guild_id=10 AND account_id=10 AND actor_id=40 AND related_id='10')
               AND EXISTS(SELECT 1 FROM economy_ledger_entries WHERE account_type='player'
                 AND account_id=40 AND actor_id=40 AND related_id='40')
               AND EXISTS(SELECT 1 FROM player_display_names WHERE discord_id=40 AND display_name='Replacement')",
            [], |r| r.get(0),
        ).unwrap();
        assert!(correct);
    }

    #[test]
    fn preserves_notification_preferences_and_survey_answers_without_old_delivery_routes() {
        let database = fast_migrated_database();
        let mut db = open_runtime_connection(database.path()).unwrap();
        db.execute_batch(
            "INSERT INTO push_notification_targets(discord_id,guild_id,ntfy_topic,readycheck_enabled,dm_match_started_enabled)
             VALUES(10,7,'cama-' || lower(hex(zeroblob(24))),0,1);
             INSERT INTO surveys(survey_id,guild_id,title,status,target_type,opened_at,created_at)
             VALUES(1,7,'Question','open','member',0,0);
             INSERT INTO survey_recipients(recipient_id,survey_id,guild_id,discord_id,delivery_status,
                         attempt_count,receipt_checked_attempt,dm_channel_id,dm_message_id,ui_channel_id,
                         ui_message_id,created_at,updated_at)
             VALUES(10,1,7,10,'sent',1,1,50,51,52,53,0,0);
             INSERT INTO survey_answers(survey_id,guild_id,recipient_id,question_id,text_value,created_at,updated_at)
             VALUES(1,7,10,99,'My answer',0,0);",
        ).unwrap();
        let tx = db.transaction().unwrap();
        transfer(&tx, 10, 40).unwrap();
        tx.commit().unwrap();
        let correct: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM push_notification_targets WHERE discord_id=40 AND ntfy_topic IS NULL
               AND readycheck_enabled=0 AND dm_match_started_enabled=1)
             AND EXISTS(SELECT 1 FROM survey_recipients r JOIN survey_answers a USING(recipient_id)
                 WHERE r.discord_id=40 AND r.recipient_id=10 AND r.delivery_status='pending'
                 AND r.dm_channel_id IS NULL AND r.dm_message_id IS NULL
                 AND r.ui_channel_id IS NULL AND r.ui_message_id IS NULL
                 AND a.text_value='My answer')", [], |r| r.get(0),
        ).unwrap();
        assert!(correct);
    }

    #[test]
    fn refuses_destination_history_even_if_replacement_never_registered() {
        let database = fast_migrated_database();
        let mut db = open_runtime_connection(database.path()).unwrap();
        db.execute_batch(
            "INSERT INTO players(discord_id,guild_id,discord_username) VALUES(10,5,'Old');
             INSERT INTO player_pairings(guild_id,player1_id,player2_id) VALUES(9,40,70);",
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        assert!(matches!(
            transfer(&tx, 10, 40),
            Err(TransferError::Conflict(_))
        ));
        tx.rollback().unwrap();
        let player: i64 = db
            .query_row("SELECT discord_id FROM players", [], |r| r.get(0))
            .unwrap();
        assert_eq!(player, 10);
    }
}
