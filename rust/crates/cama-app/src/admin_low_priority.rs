//! Player-facing low-priority copy.
//!
//! `/admin lowprio add` is the only way a player enters low priority, and it
//! notifies them. The two surfaces that show a placed player their own
//! low-priority state -- that notice and the low-priority section of
//! `/player lobby status` -- render the admin-authored reason through the one
//! normaliser here, so they redact and clip it identically.

use cama_domain::embed_safety::truncate_field;
use cama_domain::formatting::escape_discord_text;

/// Longest admin-authored reason rendered into player-facing low-priority copy.
///
/// Matches the bound `/admin lowprio add` puts on the option, so it is a `u16`
/// like the option field itself and needs no fallible conversion. Rows written
/// before that bound existed are clipped here rather than trusted, so a legacy
/// reason cannot push a DM or a status reply past Discord's 2000-character
/// message limit and get it silently rejected.
pub const REASON_DISPLAY_LIMIT: u16 = 300;

/// What a redacted user mention is replaced with in player-facing copy.
const REDACTED_MENTION: &str = "someone";

/// Prepare an admin-authored low-priority reason for player-facing copy.
///
/// Blank reasons become `None` so no dangling `Reason:` line is rendered.
///
/// User mentions are **removed**, not merely escaped. Escaping stops Discord
/// resolving `<@901>` into a display name, but it still hands the placed player
/// the reporter's raw account ID, which is one lookup away from the name. Only
/// dropping the mention outright keeps the "never say who reported them"
/// guarantee. Nothing here can stop an admin typing a name in plain prose; the
/// option description carries that warning.
///
/// What survives is escaped so its markdown cannot reformat the message around
/// it, and only then clipped to [`REASON_DISPLAY_LIMIT`] characters including
/// the truncation marker. Clipping last is what makes the budget exact:
/// escaping can double a string's length, so a clip applied first would be
/// re-inflated past the limit by the escape that follows it.
#[must_use]
pub fn player_visible_reason(reason: Option<&str>) -> Option<String> {
    let reason = reason.map(str::trim).filter(|reason| !reason.is_empty())?;
    let redacted = redact_user_mentions(reason);
    let redacted = redacted.trim();
    if redacted.is_empty() {
        return None;
    }
    Some(truncate_field(
        &escape_discord_text(redacted),
        usize::from(REASON_DISPLAY_LIMIT),
    ))
}

/// Replace every `<@id>` / `<@!id>` user mention with [`REDACTED_MENTION`].
///
/// Hand-rolled rather than pulled from a regex crate: the grammar is three
/// literal characters around a digit run, and `cama-app` has no regex
/// dependency to justify adding for it.
fn redact_user_mentions(reason: &str) -> String {
    let mut out = String::with_capacity(reason.len());
    let mut rest = reason;
    while let Some(start) = rest.find("<@") {
        let (before, candidate) = rest.split_at(start);
        let digits = candidate
            .trim_start_matches("<@")
            .trim_start_matches('!')
            .trim_start_matches('&');
        let digit_len = digits
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(digits.len());
        let closes = digits[digit_len..].starts_with('>');
        out.push_str(before);
        if digit_len > 0 && closes {
            out.push_str(REDACTED_MENTION);
            let consumed = candidate.len() - digits.len() + digit_len + 1;
            rest = &candidate[consumed..];
        } else {
            // Not a mention after all: keep the literal `<@` and carry on.
            out.push_str("<@");
            rest = &candidate[2..];
        }
    }
    out.push_str(rest);
    out
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
    fn test_lowprio_reason_removes_a_mention_rather_than_defusing_it() {
        // Escaping alone would only stop Discord resolving the mention; the raw
        // account id would still reach the player, and an id is one lookup away
        // from the name. The whole mention has to go.
        let rendered = player_visible_reason(Some("reported by <@901> for griefing"))
            .expect("reason survives");
        assert!(!rendered.contains("<@901>"));
        assert!(
            !rendered.contains("901"),
            "the reporter's id must not survive: {rendered}"
        );
        assert!(rendered.contains("for griefing"), "the rest is kept");

        let notice = assignment_direct_message(3, Some("reported by <@901> for griefing"), None);
        assert!(!notice.contains("<@901>"));
        assert!(notice.contains("Reason: reported by someone for griefing"));
    }

    #[test]
    fn test_lowprio_reason_redacts_every_mention_shape_and_keeps_the_rest() {
        for mention in ["<@901>", "<@!901>", "<@&901>"] {
            let rendered =
                player_visible_reason(Some(&format!("ganked by {mention} twice"))).unwrap();
            assert_eq!(rendered, "ganked by someone twice", "shape {mention}");
        }
        assert_eq!(
            player_visible_reason(Some("<@1> and <@2> both reported it")).unwrap(),
            "someone and someone both reported it"
        );
        // Text that only looks like the start of a mention is left alone.
        let literal = player_visible_reason(Some("said <@ nothing")).unwrap();
        assert!(literal.contains("nothing"), "{literal}");
        assert!(!literal.contains("someone"), "{literal}");
    }

    #[test]
    fn test_lowprio_reason_that_is_only_a_mention_keeps_no_trace_of_the_id() {
        let notice = assignment_direct_message(3, Some("<@901>"), None);
        assert!(notice.contains("Reason: someone"), "{notice}");
        assert!(!notice.contains("901"), "{notice}");
    }

    #[test]
    fn test_lowprio_reason_is_clipped_so_a_legacy_row_cannot_break_delivery() {
        let legacy = "g".repeat(4_000);
        let rendered = player_visible_reason(Some(&legacy)).expect("reason survives");
        assert_eq!(
            rendered.chars().count(),
            usize::from(REASON_DISPLAY_LIMIT),
            "the marker fits inside the documented budget"
        );
        assert!(rendered.ends_with("..."));

        let notice = assignment_direct_message(3, Some(&legacy), None);
        assert!(
            notice.chars().count() < 2_000,
            "the notice must stay inside Discord's message limit, got {}",
            notice.chars().count()
        );
    }

    #[test]
    fn test_lowprio_reason_at_the_limit_is_not_marked_as_clipped() {
        let exact = "g".repeat(usize::from(REASON_DISPLAY_LIMIT));
        let rendered = player_visible_reason(Some(&exact)).expect("reason survives");
        assert_eq!(rendered, exact);
        assert!(!rendered.ends_with("..."));
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
