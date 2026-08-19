//! Player-facing low-priority copy.
//!
//! `/admin lowprio add` is the only way a player enters low priority, and it
//! notifies them. Both surfaces that show a placed player their own state --
//! that notice and `/player lobby status` -- render the admin-authored reason
//! through the one normaliser here so they clip and defuse it identically.

use cama_domain::formatting::escape_discord_text;

/// Longest admin-authored reason rendered into player-facing low-priority copy.
///
/// Matches the bound `/admin lowprio add` puts on the option. Rows written
/// before that bound existed are clipped here rather than trusted, so a legacy
/// reason cannot push a DM or a status reply past Discord's 2000-character
/// message limit and get it silently rejected.
pub const REASON_DISPLAY_LIMIT: usize = 300;

/// Prepare an admin-authored low-priority reason for player-facing copy.
///
/// Blank reasons become `None` so no dangling `Reason:` line is rendered. The
/// surviving text is clipped to [`REASON_DISPLAY_LIMIT`] and then escaped, which
/// matters for more than markdown: `escape_discord_text` breaks up `@`, so a
/// reason an admin wrote as `reported by <@901>` reaches the player as inert
/// text instead of resolving into the reporter's name. Escaping cannot stop an
/// admin from typing a name in plain prose — the option description carries that
/// warning — but it does stop the mention syntax from doing it for them.
#[must_use]
pub fn player_visible_reason(reason: Option<&str>) -> Option<String> {
    let reason = reason.map(str::trim).filter(|reason| !reason.is_empty())?;
    let mut clipped: String = reason.chars().take(REASON_DISPLAY_LIMIT).collect();
    if clipped.chars().count() < reason.chars().count() {
        clipped.push('…');
    }
    Some(escape_discord_text(&clipped))
}

/// Polite notice sent to a player when an admin places them in low priority.
///
/// The notice carries the admin-authored reason so the player learns what the
/// correction is for, but never the issuing admin: `set_by` stays admin-facing
/// on `/admin lowprio status`. `low_priority_state` has no reporter column, so
/// the reason is the only field that could name whoever raised the behaviour;
/// it goes through [`player_visible_reason`] first.
#[must_use]
pub fn assignment_direct_message(
    wins_required: i64,
    reason: Option<&str>,
    guild_name: Option<&str>,
) -> String {
    let wins = if wins_required == 1 { "win" } else { "wins" };
    let games = if wins_required == 1 { "game" } else { "games" };
    let reason = player_visible_reason(reason)
        .map_or_else(String::new, |reason| format!("\nReason: {reason}"));
    // Low priority is per-guild but a DM is not, so name the server when the
    // transport can resolve it rather than leaving a two-league player guessing.
    let server = guild_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(
            || "in the server".to_owned(),
            |name| format!("in **{}**", escape_discord_text(name)),
        );
    format!(
        "You were placed in low priority {server} for **{wins_required} {wins}**.\n\
         The matchmaker is asking for a small course correction.\
         {reason}\n\
         Win {wins_required} recorded {games} to return to regular matchmaking.\n\
         Use `/player lobby status` {server} to view your progress."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lowprio_assignment_dm_is_polite_and_points_at_the_player_status_command() {
        assert_eq!(
            assignment_direct_message(3, Some("repeated abandons"), Some("Camaraderous")),
            "You were placed in low priority in **Camaraderous** for **3 wins**.\n\
             The matchmaker is asking for a small course correction.\n\
             Reason: repeated abandons\n\
             Win 3 recorded games to return to regular matchmaking.\n\
             Use `/player lobby status` in **Camaraderous** to view your progress."
        );
        assert_eq!(
            assignment_direct_message(1, None, None),
            "You were placed in low priority in the server for **1 win**.\n\
             The matchmaker is asking for a small course correction.\n\
             Win 1 recorded game to return to regular matchmaking.\n\
             Use `/player lobby status` in the server to view your progress."
        );
    }

    #[test]
    fn test_lowprio_reason_defuses_a_mention_so_it_cannot_resolve_to_a_reporter() {
        // Suppressed mentions do not stop Discord rendering `<@id>` as the
        // named user, so the syntax itself has to be broken up.
        let rendered = player_visible_reason(Some("reported by <@901> for griefing"))
            .expect("reason survives");
        assert!(!rendered.contains("<@901>"));
        assert!(rendered.contains('\u{200b}'));
        assert!(
            rendered.contains("901"),
            "the text is defused, not censored"
        );

        let notice = assignment_direct_message(3, Some("reported by <@901> for griefing"), None);
        assert!(!notice.contains("<@901>"));
        // `@` gains a zero-width space and `>` is escaped, so Discord's mention
        // parser no longer sees a user reference to resolve.
        assert!(notice.contains("Reason: reported by <@\u{200b}901\\>"));
    }

    #[test]
    fn test_lowprio_reason_is_clipped_so_a_legacy_row_cannot_break_delivery() {
        let legacy = "g".repeat(4_000);
        let rendered = player_visible_reason(Some(&legacy)).expect("reason survives");
        assert_eq!(rendered.chars().count(), REASON_DISPLAY_LIMIT + 1);
        assert!(rendered.ends_with('…'));

        let notice = assignment_direct_message(3, Some(&legacy), None);
        assert!(
            notice.chars().count() < 2_000,
            "the notice must stay inside Discord's message limit, got {}",
            notice.chars().count()
        );
    }

    #[test]
    fn test_lowprio_reason_at_the_limit_is_not_marked_as_clipped() {
        let exact = "g".repeat(REASON_DISPLAY_LIMIT);
        let rendered = player_visible_reason(Some(&exact)).expect("reason survives");
        assert_eq!(rendered, exact);
        assert!(!rendered.ends_with('…'));
    }

    #[test]
    fn test_lowprio_assignment_dm_omits_a_blank_reason_rather_than_an_empty_line() {
        for blank in ["", "   ", "\n\t "] {
            let notice = assignment_direct_message(2, Some(blank), None);
            assert!(
                !notice.contains("Reason"),
                "blank reason {blank:?} rendered"
            );
            assert_eq!(notice, assignment_direct_message(2, None, None));
        }
    }

    #[test]
    fn test_lowprio_assignment_dm_carries_the_reason_but_never_the_issuing_admin() {
        let notice = assignment_direct_message(4, Some("repeated abandons"), None);
        assert!(notice.contains("**4 wins**"));
        assert!(notice.contains("Reason: repeated abandons"));
        // The issuing admin is not an input here by construction: `set_by` stays
        // on the admin-facing `/admin lowprio status`. The player is told what,
        // never who.
        assert!(!notice.contains("<@"));
    }

    #[test]
    fn test_lowprio_assignment_dm_pluralises_every_legal_win_count() {
        for wins in 1..=20 {
            let notice = assignment_direct_message(wins, None, None);
            let (win_noun, game_noun) = if wins == 1 {
                ("1 win**", "Win 1 recorded game ")
            } else {
                ("wins**", "recorded games ")
            };
            assert!(notice.contains(win_noun), "wins={wins}: {notice}");
            assert!(notice.contains(game_noun), "wins={wins}: {notice}");
        }
    }
}
