use super::*;

fn kick(discord_id: i64, guild_id: i64, lobby_kind: LobbyKind, window_name: &str) -> CurfewKick {
    CurfewKick {
        discord_id,
        guild_id,
        lobby_kind,
        window_name: window_name.to_owned(),
    }
}

#[test]
fn single_lobby_kick_names_that_lobby() {
    let notices = curfew_kick_notices(&[kick(7, 1, LobbyKind::LowSkill, "sleep")]);
    assert_eq!(
        notices,
        vec![(
            7,
            "🔒 Your \"sleep\" curfew window started, so you've been removed from 🧀 Whine & Cheese.".to_owned()
        )]
    );
}

#[test]
fn kicks_from_both_lobbies_collapse_into_one_dm() {
    let notices = curfew_kick_notices(&[
        kick(7, 1, LobbyKind::Open, "sleep_and_performance"),
        kick(7, 1, LobbyKind::LowSkill, "sleep_and_performance"),
    ]);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, 7);
    assert!(
        notices[0]
            .1
            .contains("removed from 🍽️ All You Can Feed and 🧀 Whine & Cheese."),
        "{}",
        notices[0].1
    );
}

#[test]
fn same_lobby_kind_across_guilds_is_not_repeated() {
    let notices = curfew_kick_notices(&[
        kick(7, 1, LobbyKind::Open, "sleep"),
        kick(7, 2, LobbyKind::Open, "sleep"),
    ]);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].1.contains("removed from 🍽️ All You Can Feed."));
}

#[test]
fn different_players_and_windows_get_separate_dms() {
    let notices = curfew_kick_notices(&[
        kick(7, 1, LobbyKind::Open, "sleep"),
        kick(9, 1, LobbyKind::Open, "work"),
        kick(7, 2, LobbyKind::LowSkill, "study"),
    ]);
    assert_eq!(notices.len(), 3);
    assert_eq!(notices[0].0, 7);
    assert!(notices[0].1.contains("\"sleep\""));
    assert_eq!(notices[1].0, 7);
    assert!(notices[1].1.contains("\"study\""));
    assert_eq!(notices[2].0, 9);
    assert!(notices[2].1.contains("\"work\""));
}
