SELECT w.discord_id, w.guild_id, w.bet_id, 'dota:' || w.match_id AS match_id,
       w.bet_time, w.effective_bet, w.leverage, w.won,
       CASE WHEN w.won THEN COALESCE(w.payout, w.effective_bet * 2) - w.effective_bet
            ELSE -w.effective_bet END
         - COALESCE(t.vanity_tax + t.low_priority_tax + t.bankruptcy_penalty, 0) AS net_profit
FROM (
    SELECT b.discord_id, b.guild_id, b.bet_id, b.match_id, b.bet_time, b.team_bet_on,
           b.amount * COALESCE(b.leverage, 1) AS effective_bet,
           COALESCE(b.leverage, 1) AS leverage, b.payout,
           CASE WHEN (m.winning_team = 1 AND b.team_bet_on = 'radiant')
                  OR (m.winning_team = 2 AND b.team_bet_on = 'dire') THEN 1 ELSE 0 END AS won
    FROM bets AS b
    JOIN matches AS m ON m.match_id = b.match_id AND m.guild_id = b.guild_id
    WHERE m.winning_team IS NOT NULL
) AS w
LEFT JOIN bet_settlement_taxes AS t
  ON t.match_id = w.match_id AND t.guild_id = w.guild_id AND t.discord_id = w.discord_id
 AND w.won = 1
 -- Match player_bet_history's attribution. A date range can split the wagers
 -- for one match, but only the first winning wager carries its deductions.
 AND w.bet_id = (
     SELECT first_win.bet_id FROM bets AS first_win
     WHERE first_win.guild_id = t.guild_id AND first_win.match_id = t.match_id
       AND first_win.discord_id = t.discord_id AND first_win.team_bet_on = w.team_bet_on
     ORDER BY first_win.bet_time, first_win.bet_id LIMIT 1
 )
