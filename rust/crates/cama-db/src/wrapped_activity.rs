//! Read-only yearly projections of Dig, Camagotchi, Mafia, and side-gamba
//! activity for `/wrapped`.
//!
//! Every query is scoped to one guild and a `[start, end)` Unix-second window
//! and never mutates state. Match bets come from the gambling statistics port,
//! which owns their settlement math.

use std::path::{Path, PathBuf};

use cama_domain::wrapped_activity::{
    BossRecord, DigYear, MafiaYear, PersonTally, PetRecord, PetYear, ServerRank, TipYear, WheelYear,
};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Debug, Error)]
pub enum WrappedActivityError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WrappedActivityRepository {
    path: PathBuf,
}

/// Inclusive start and exclusive end of the Wrapped year in Unix seconds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct YearWindow {
    pub start: i64,
    pub end: i64,
}

const DIG_KINDS: &str = "('dig','paid_dig')";

fn totals(
    connection: &Connection,
    sql: &str,
    guild_id: i64,
    window: YearWindow,
) -> Result<Vec<(i64, i64)>, rusqlite::Error> {
    connection
        .prepare(sql)?
        .query_map(params![guild_id, window.start, window.end], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect()
}

fn top_person(
    connection: &Connection,
    sql: &str,
    arguments: impl rusqlite::Params,
) -> Result<Option<PersonTally>, rusqlite::Error> {
    connection
        .query_row(sql, arguments, |row| {
            Ok(PersonTally {
                discord_id: row.get(0)?,
                count: row.get(1)?,
            })
        })
        .optional()
}

impl WrappedActivityRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn dig_year(
        &self,
        guild_id: i64,
        discord_id: i64,
        window: YearWindow,
    ) -> Result<DigYear, WrappedActivityError> {
        let connection = open_runtime_connection(&self.path)?;
        let arguments = params![guild_id, discord_id, window.start, window.end];
        let mut year = connection.query_row(
            &format!(
                "SELECT COUNT(*),
                        COALESCE(SUM(action_type = 'paid_dig'), 0),
                        COALESCE(SUM(MAX(depth_after - depth_before, 0)), 0),
                        COALESCE(MAX(depth_after), 0),
                        COALESCE(SUM(jc_delta), 0),
                        COALESCE(SUM(json_valid(detail) AND json_extract(detail, '$.cave_in') = 1), 0),
                        COALESCE(SUM(json_valid(detail) AND json_type(detail, '$.event') = 'text'), 0),
                        COUNT(DISTINCT date(created_at, 'unixepoch'))
                   FROM dig_actions
                  WHERE guild_id = ?1 AND actor_id = ?2
                    AND created_at >= ?3 AND created_at < ?4
                    AND action_type IN {DIG_KINDS}"
            ),
            arguments,
            |row| {
                Ok(DigYear {
                    digs: row.get(0)?,
                    paid_digs: row.get(1)?,
                    blocks_dug: row.get(2)?,
                    deepest: row.get(3)?,
                    jc_from_digs: row.get(4)?,
                    cave_ins: row.get(5)?,
                    events: row.get(6)?,
                    active_days: row.get(7)?,
                    ..DigYear::default()
                })
            },
        )?;
        year.busiest_day = connection
            .query_row(
                &format!(
                    "SELECT date(created_at, 'unixepoch') AS day, COUNT(*) AS digs
                       FROM dig_actions
                      WHERE guild_id = ?1 AND actor_id = ?2
                        AND created_at >= ?3 AND created_at < ?4
                        AND action_type IN {DIG_KINDS}
                   GROUP BY day ORDER BY digs DESC, day ASC LIMIT 1"
                ),
                arguments,
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        year.bosses = connection
            .prepare(
                "SELECT json_extract(detail, '$.boss_id') AS boss,
                        SUM(json_extract(detail, '$.won') = 1),
                        SUM(COALESCE(json_extract(detail, '$.won'), 0) != 1)
                   FROM dig_actions
                  WHERE guild_id = ?1 AND actor_id = ?2
                    AND created_at >= ?3 AND created_at < ?4
                    AND action_type IN ('boss_fight', 'pinnacle_fight')
                    AND json_valid(detail) AND json_type(detail, '$.boss_id') = 'text'
               GROUP BY boss ORDER BY COUNT(*) DESC, boss ASC",
            )?
            .query_map(arguments, |row| {
                Ok(BossRecord {
                    boss_id: row.get(0)?,
                    wins: row.get(1)?,
                    losses: row.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        (
            year.sabotages_sent,
            year.sabotages_received,
            year.helps_given,
            year.helps_received,
        ) = connection.query_row(
            "SELECT COALESCE(SUM(action_type = 'sabotage' AND actor_id = ?2), 0),
                        COALESCE(SUM(action_type = 'sabotage' AND target_id = ?2), 0),
                        COALESCE(SUM(action_type = 'help' AND actor_id = ?2), 0),
                        COALESCE(SUM(action_type = 'help' AND target_id = ?2), 0)
                   FROM dig_actions
                  WHERE guild_id = ?1 AND (actor_id = ?2 OR target_id = ?2)
                    AND created_at >= ?3 AND created_at < ?4",
            arguments,
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        year.favorite_target = top_person(
            &connection,
            "SELECT target_id, COUNT(*) AS times FROM dig_actions
              WHERE guild_id = ?1 AND actor_id = ?2 AND action_type = 'sabotage'
                AND target_id IS NOT NULL AND target_id != ?2
                AND created_at >= ?3 AND created_at < ?4
           GROUP BY target_id ORDER BY times DESC, target_id ASC LIMIT 1",
            arguments,
        )?;
        year.top_saboteur = top_person(
            &connection,
            "SELECT actor_id, COUNT(*) AS times FROM dig_actions
              WHERE guild_id = ?1 AND target_id = ?2 AND action_type = 'sabotage'
                AND actor_id != ?2
                AND created_at >= ?3 AND created_at < ?4
           GROUP BY actor_id ORDER BY times DESC, actor_id ASC LIMIT 1",
            arguments,
        )?;
        (year.artifacts, year.relics) = connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(is_relic != 0), 0) FROM dig_artifacts
              WHERE guild_id = ?1 AND discord_id = ?2
                AND found_at >= ?3 AND found_at < ?4",
            arguments,
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        year.first_finds = connection.query_row(
            "SELECT COUNT(*) FROM dig_artifact_registry
              WHERE guild_id = ?1 AND first_finder_id = ?2
                AND first_found_at >= ?3 AND first_found_at < ?4",
            arguments,
            |row| row.get(0),
        )?;
        year.achievements = connection.query_row(
            "SELECT COUNT(*) FROM dig_achievements
              WHERE guild_id = ?1 AND discord_id = ?2
                AND unlocked_at >= ?3 AND unlocked_at < ?4",
            arguments,
            |row| row.get(0),
        )?;
        if let Some((prestige, name)) = connection
            .query_row(
                "SELECT prestige_level, tunnel_name FROM tunnels
                  WHERE guild_id = ?1 AND discord_id = ?2",
                params![guild_id, discord_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
        {
            year.prestige_level = prestige;
            year.tunnel_name = name;
        }
        year.rank = ServerRank::among(
            &totals(
                &connection,
                &format!(
                    "SELECT actor_id, COUNT(*) FROM dig_actions
                      WHERE guild_id = ?1 AND created_at >= ?2 AND created_at < ?3
                        AND action_type IN {DIG_KINDS}
                   GROUP BY actor_id"
                ),
                guild_id,
                window,
            )?,
            discord_id,
        );
        Ok(year)
    }

    pub fn pet_year(
        &self,
        guild_id: i64,
        discord_id: i64,
        window: YearWindow,
    ) -> Result<PetYear, WrappedActivityError> {
        let connection = open_runtime_connection(&self.path)?;
        let arguments = params![guild_id, discord_id, window.start, window.end];
        let alive_during = "adopted_at < ?4 AND (died_at IS NULL OR died_at >= ?3)";
        let pets = connection
            .prepare(&format!(
                "SELECT pet_id, name, species, adopted_at, died_at, death_cause,
                        times_fed, is_active
                   FROM pets
                  WHERE guild_id = ?1 AND discord_id = ?2 AND {alive_during}
               ORDER BY adopted_at ASC, pet_id ASC"
            ))?
            .query_map(arguments, |row| {
                Ok(PetRecord {
                    pet_id: row.get(0)?,
                    name: row.get(1)?,
                    species: row.get(2)?,
                    adopted_at: row.get(3)?,
                    died_at: row.get(4)?,
                    death_cause: row.get(5)?,
                    times_fed: row.get(6)?,
                    is_active: row.get::<_, i64>(7)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let jc_spent = connection.query_row(
            "SELECT COALESCE(SUM(-delta), 0) FROM economy_ledger_entries
              WHERE guild_id = ?1 AND account_type = 'player' AND account_id = ?2
                AND source = 'pet' AND delta < 0
                AND created_at >= ?3 AND created_at < ?4",
            arguments,
            |row| row.get(0),
        )?;
        let (brawl_wins, brawl_losses, brawl_draws) = connection.query_row(
            "SELECT COALESCE(SUM(winner_id = ?2), 0),
                    COALESCE(SUM(winner_id IS NOT NULL AND winner_id != ?2), 0),
                    COALESCE(SUM(winner_id IS NULL), 0)
               FROM pet_brawls
              WHERE guild_id = ?1 AND status = 'done'
                AND (challenger_id = ?2 OR recipient_id = ?2)
                AND resolved_at >= ?3 AND resolved_at < ?4",
            arguments,
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let brawl_rival = top_person(
            &connection,
            "SELECT CASE WHEN challenger_id = ?2 THEN recipient_id ELSE challenger_id END AS rival,
                    COUNT(*) AS brawls
               FROM pet_brawls
              WHERE guild_id = ?1 AND status = 'done'
                AND (challenger_id = ?2 OR recipient_id = ?2)
                AND challenger_id != recipient_id
                AND resolved_at >= ?3 AND resolved_at < ?4
           GROUP BY rival ORDER BY brawls DESC, rival ASC LIMIT 1",
            arguments,
        )?;
        let keepsakes = connection.query_row(
            "SELECT COUNT(*) FROM pet_keepsakes
              WHERE guild_id = ?1 AND discord_id = ?2
                AND received_at >= ?3 AND received_at < ?4",
            arguments,
            |row| row.get(0),
        )?;
        let rank = ServerRank::among(
            &totals(
                &connection,
                "SELECT discord_id, SUM(times_fed) FROM pets
                  WHERE guild_id = ?1 AND adopted_at < ?3
                    AND (died_at IS NULL OR died_at >= ?2)
               GROUP BY discord_id",
                guild_id,
                window,
            )?,
            discord_id,
        );
        Ok(PetYear {
            pets,
            jc_spent,
            brawl_wins,
            brawl_losses,
            brawl_draws,
            brawl_rival,
            keepsakes,
            rank,
        })
    }

    pub fn mafia_year(
        &self,
        guild_id: i64,
        discord_id: i64,
        window: YearWindow,
    ) -> Result<MafiaYear, WrappedActivityError> {
        let connection = open_runtime_connection(&self.path)?;
        let arguments = params![guild_id, discord_id, window.start, window.end];
        // Settled games only: an unfinished game must not leak its roles.
        let settled = "g.guild_id = ?1 AND g.phase = 'RESOLVED' AND g.status != 'CANCELLED'
                       AND g.started_at >= ?3 AND g.started_at < ?4";
        let town = "('TOWNIE','DOCTOR','DETECTIVE','VIGILANTE')";
        let mut year = connection.query_row(
            &format!(
                "SELECT COUNT(*),
                        COALESCE(SUM((g.winner = 'TOWN' AND p.role IN {town})
                                  OR (g.winner = 'MAFIA' AND p.role = 'MAFIA')
                                  OR (g.winner = 'JESTER' AND p.role = 'JESTER')), 0),
                        COALESCE(SUM(p.role = 'MAFIA'), 0),
                        COALESCE(SUM(p.role = 'MAFIA' AND g.winner = 'MAFIA'), 0),
                        COALESCE(SUM(p.role IN {town}), 0),
                        COALESCE(SUM(p.role IN {town} AND g.winner = 'TOWN'), 0),
                        COALESCE(SUM(p.role = 'JESTER'), 0),
                        COALESCE(SUM(p.role = 'JESTER' AND g.winner = 'JESTER'), 0),
                        COALESCE(SUM(p.is_godfather != 0), 0),
                        COALESCE(SUM(p.role = 'DOCTOR'), 0),
                        COALESCE(SUM(p.role = 'DETECTIVE'), 0),
                        COALESCE(SUM(g.mvp_id = p.discord_id), 0),
                        COALESCE(SUM(p.is_alive != 0), 0),
                        COALESCE(SUM(p.eliminated_phase = 'NIGHT'), 0),
                        COALESCE(SUM(p.eliminated_phase = 'DAY'), 0),
                        COALESCE(SUM(p.eliminated_phase = 'NIGHT' AND EXISTS (
                            SELECT 1 FROM mafia_actions k
                             WHERE k.game_id = g.game_id AND k.target_id = p.discord_id
                               AND k.action_type IN ('KILL','VIG_KILL') AND k.day_number = 1)), 0),
                        COALESCE(SUM(CASE WHEN (g.winner = 'TOWN' AND p.role IN {town})
                                            OR (g.winner = 'MAFIA' AND p.role = 'MAFIA')
                                            OR (g.winner = 'JESTER' AND p.role = 'JESTER')
                                          THEN g.payout_per_winner ELSE 0 END), 0)
                   FROM mafia_players p JOIN mafia_games g ON g.game_id = p.game_id
                  WHERE p.discord_id = ?2 AND {settled}"
            ),
            arguments,
            |row| {
                Ok(MafiaYear {
                    games: row.get(0)?,
                    wins: row.get(1)?,
                    mafia_games: row.get(2)?,
                    mafia_wins: row.get(3)?,
                    town_games: row.get(4)?,
                    town_wins: row.get(5)?,
                    jester_games: row.get(6)?,
                    jester_wins: row.get(7)?,
                    godfather_games: row.get(8)?,
                    doctor_games: row.get(9)?,
                    detective_games: row.get(10)?,
                    mvps: row.get(11)?,
                    survived: row.get(12)?,
                    killed_at_night: row.get(13)?,
                    voted_out: row.get(14)?,
                    first_night_deaths: row.get(15)?,
                    jc_won: row.get(16)?,
                    ..MafiaYear::default()
                })
            },
        )?;
        if year.games == 0 {
            return Ok(year);
        }
        // The same definitions as the lifetime Mafia stats, inside the year.
        (year.kills, year.saves, year.correct_reads) = connection.query_row(
            &format!(
                "SELECT
                   (SELECT COUNT(*) FROM mafia_actions a
                      JOIN mafia_players victim ON victim.game_id = a.game_id AND victim.discord_id = a.target_id
                      JOIN mafia_games g ON g.game_id = a.game_id
                     WHERE a.actor_id = ?2 AND a.action_type = 'KILL'
                       AND victim.eliminated_phase = 'NIGHT' AND {settled}),
                   (SELECT COUNT(*) FROM mafia_actions s
                      JOIN mafia_players target ON target.game_id = s.game_id AND target.discord_id = s.target_id
                      JOIN mafia_games g ON g.game_id = s.game_id
                     WHERE s.actor_id = ?2 AND s.action_type = 'SAVE'
                       AND target.eliminated_phase IS NULL AND EXISTS (
                           SELECT 1 FROM mafia_actions k WHERE k.game_id = s.game_id
                              AND k.action_type = 'KILL' AND k.target_id = s.target_id)
                       AND {settled}),
                   (SELECT COUNT(*) FROM mafia_actions i
                      JOIN mafia_players target ON target.game_id = i.game_id AND target.discord_id = i.target_id
                      JOIN mafia_games g ON g.game_id = i.game_id
                     WHERE i.actor_id = ?2 AND i.action_type = 'INVESTIGATE'
                       AND target.role = 'MAFIA' AND target.is_godfather = 0
                       AND target.eliminated_phase = 'DAY' AND {settled})"
            ),
            arguments,
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        (year.votes_cast, year.votes_on_mafia) = connection.query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(target.role = 'MAFIA'), 0)
                   FROM mafia_actions v
                   JOIN mafia_games g ON g.game_id = v.game_id
              LEFT JOIN mafia_players target ON target.game_id = v.game_id AND target.discord_id = v.target_id
                  WHERE v.actor_id = ?2 AND v.action_type = 'VOTE' AND v.target_id IS NOT NULL
                    AND {settled}"
            ),
            arguments,
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        year.favorite_victim = top_person(
            &connection,
            &format!(
                "SELECT a.target_id, COUNT(*) AS times FROM mafia_actions a
                   JOIN mafia_games g ON g.game_id = a.game_id
                  WHERE a.actor_id = ?2 AND a.action_type IN ('KILL','VIG_KILL')
                    AND a.target_id IS NOT NULL AND a.target_id != ?2 AND {settled}
               GROUP BY a.target_id ORDER BY times DESC, a.target_id ASC LIMIT 1"
            ),
            arguments,
        )?;
        year.top_accuser = top_person(
            &connection,
            &format!(
                "SELECT v.actor_id, COUNT(*) AS times FROM mafia_actions v
                   JOIN mafia_games g ON g.game_id = v.game_id
                  WHERE v.target_id = ?2 AND v.action_type = 'VOTE'
                    AND v.actor_id != ?2 AND {settled}
               GROUP BY v.actor_id ORDER BY times DESC, v.actor_id ASC LIMIT 1"
            ),
            arguments,
        )?;
        year.bounties_placed = connection.query_row(
            &format!(
                "SELECT COALESCE(SUM(b.amount), 0) FROM mafia_bounties b
                   JOIN mafia_games g ON g.game_id = b.game_id
                  WHERE b.contributor_id = ?2 AND {settled}"
            ),
            arguments,
            |row| row.get(0),
        )?;
        year.favorite_alias = connection
            .query_row(
                &format!(
                    "SELECT p.hero_name, COUNT(*) AS times
                       FROM mafia_players p JOIN mafia_games g ON g.game_id = p.game_id
                      WHERE p.discord_id = ?2 AND p.hero_name IS NOT NULL AND {settled}
                   GROUP BY p.hero_name HAVING times > 1
                   ORDER BY times DESC, p.hero_name ASC LIMIT 1"
                ),
                arguments,
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        year.rank = ServerRank::among(
            &totals(
                &connection,
                "SELECT p.discord_id, COUNT(*)
                   FROM mafia_players p JOIN mafia_games g ON g.game_id = p.game_id
                  WHERE g.guild_id = ?1 AND g.phase = 'RESOLVED' AND g.status != 'CANCELLED'
                    AND g.started_at >= ?2 AND g.started_at < ?3
               GROUP BY p.discord_id",
                guild_id,
                window,
            )?,
            discord_id,
        );
        Ok(year)
    }

    /// Wheel spins, tips, and the player's place among match bettors. Match bet
    /// outcomes themselves come from the gambling statistics port.
    pub fn gamba_side_year(
        &self,
        guild_id: i64,
        discord_id: i64,
        window: YearWindow,
    ) -> Result<(WheelYear, TipYear, Option<ServerRank>), WrappedActivityError> {
        let connection = open_runtime_connection(&self.path)?;
        let arguments = params![guild_id, discord_id, window.start, window.end];
        let wheel = connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(result), 0), COALESCE(MAX(result), 0),
                    COALESCE(MIN(result), 0), COALESCE(SUM(is_bankrupt != 0), 0),
                    COALESCE(SUM(is_golden != 0), 0)
               FROM wheel_spins
              WHERE guild_id = ?1 AND discord_id = ?2
                AND spin_time >= ?3 AND spin_time < ?4",
            arguments,
            |row| {
                Ok(WheelYear {
                    spins: row.get(0)?,
                    net: row.get(1)?,
                    best: row.get(2)?,
                    worst: row.get(3)?,
                    bankrupts: row.get(4)?,
                    golden: row.get(5)?,
                })
            },
        )?;
        let (sent, sent_jc, received, received_jc) = connection.query_row(
            "SELECT COALESCE(SUM(sender_id = ?2), 0),
                    COALESCE(SUM(CASE WHEN sender_id = ?2 THEN amount ELSE 0 END), 0),
                    COALESCE(SUM(recipient_id = ?2), 0),
                    COALESCE(SUM(CASE WHEN recipient_id = ?2 THEN amount ELSE 0 END), 0)
               FROM tip_transactions
              WHERE guild_id = ?1 AND (sender_id = ?2 OR recipient_id = ?2)
                AND sender_id != recipient_id
                AND timestamp >= ?3 AND timestamp < ?4",
            arguments,
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let top_recipient = top_person(
            &connection,
            "SELECT recipient_id, SUM(amount) AS total FROM tip_transactions
              WHERE guild_id = ?1 AND sender_id = ?2 AND recipient_id != ?2
                AND timestamp >= ?3 AND timestamp < ?4
           GROUP BY recipient_id ORDER BY total DESC, recipient_id ASC LIMIT 1",
            arguments,
        )?;
        let rank = ServerRank::among(
            &totals(
                &connection,
                "SELECT discord_id, SUM(amount * COALESCE(leverage, 1)) FROM bets
                  WHERE guild_id = ?1 AND bet_time >= ?2 AND bet_time < ?3
               GROUP BY discord_id",
                guild_id,
                window,
            )?,
            discord_id,
        );
        Ok((
            wheel,
            TipYear {
                sent,
                sent_jc,
                received,
                received_jc,
                top_recipient,
            },
            rank,
        ))
    }
}

#[cfg(test)]
#[path = "wrapped_activity/tests.rs"]
mod tests;
