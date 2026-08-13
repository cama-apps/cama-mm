use super::*;

#[test]
fn test_abort_lobby_thread_no_long_sleep() {
    let plan = abort_lobby_thread_plan();
    assert_eq!(plan.len(), 2);
    assert!(matches!(plan[0], ThreadAction::Send(_)));
    assert!(matches!(plan[1], ThreadAction::RenameAndArchive { .. }));
}

#[test]
fn test_thread_id_stored_when_thread_post_fails() {
    let plan = lock_lobby_thread_plan(123, 77, 424_242);
    assert_eq!(
        plan[0],
        ThreadAction::StorePendingThread {
            guild_id: 123,
            pending_match_id: 77,
            thread_id: 424_242,
        }
    );
    // A transport adapter may fail on this later operation; storage already
    // occupies the first, independently executable step.
    assert!(matches!(plan[1], ThreadAction::Send(_)));
}

#[test]
fn test_no_fallback_to_other_matchs_thread() {
    let unrelated_unscoped_thread = Some(999);
    assert_eq!(recorded_match_thread(None, None), None);
    assert_eq!(recorded_match_thread(Some(777), None), Some(777));
    assert_ne!(recorded_match_thread(None, None), unrelated_unscoped_thread);
}

#[test]
fn test_gambling_leaderboard_uses_guild_members_cache() {
    let members = [
        GuildMember {
            discord_id: 1_001,
            display_name: "CachedUser1".to_owned(),
        },
        GuildMember {
            discord_id: 1_002,
            display_name: "CachedUser2".to_owned(),
        },
    ];
    let view = GamblingLeaderboardView::new(
        Some(&members),
        vec![GamblingEntry {
            discord_id: 1_001,
            net_pnl: 50,
        }],
    );
    assert_eq!(view.member_cache().len(), 2);
    assert!(view.render_lines()[0].contains("CachedUser1"));
}

#[test]
fn test_gambling_leaderboard_handles_missing_guild() {
    let view = GamblingLeaderboardView::new(
        None,
        vec![GamblingEntry {
            discord_id: 1_001,
            net_pnl: -20,
        }],
    );
    assert!(view.member_cache().is_empty());
    assert_eq!(view.get_name(1_001), "User 1001");
    assert_eq!(view.render_lines(), ["User 1001: -20 JC"]);
}

#[test]
fn test_teammates_tab_has_spacer_after_most_played_against() {
    let fields = teammates_field_layout();
    let spacers = fields
        .iter()
        .filter(|field| field.name == "\u{200b}" && field.value == "\u{200b}")
        .count();
    assert_eq!(spacers, 4);
    assert_eq!(fields[7].name, "Most Played Against");
    assert_eq!(fields[8].name, "\u{200b}");
}

#[test]
fn test_teammates_tab_field_order_correct() {
    let names = teammates_field_layout()
        .into_iter()
        .filter(|field| field.name != "\u{200b}")
        .map(|field| field.name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "Best Teammates",
            "Worst Teammates",
            "Dominates",
            "Struggles Against",
            "Most Played With",
            "Most Played Against",
            "Even Teammates",
            "Even Opponents",
        ]
    );
}

#[test]
fn test_get_name_is_sync_in_gambling_leaderboard() {
    let view = GamblingLeaderboardView::new(
        Some(&[GuildMember {
            discord_id: 7,
            display_name: "Seven".to_owned(),
        }]),
        Vec::new(),
    );
    let synchronous_lookup: fn(&GamblingLeaderboardView, i64) -> String =
        GamblingLeaderboardView::get_name;
    assert_eq!(synchronous_lookup(&view, 7), "Seven");
}

#[test]
fn test_critical_extensions_in_extensions_list() {
    assert!(EXTENSIONS.contains(&"commands.info"));
    assert!(EXTENSIONS.contains(&"commands.profile"));
}

#[test]
fn test_info_commands_module_imports_without_error() {
    assert!(source_supports_contract(INFO_COG));
}

#[test]
fn test_profile_commands_module_imports_without_error() {
    assert!(source_supports_contract(PROFILE_COG));
}

#[test]
fn test_dota_info_commands_module_imports_without_error() {
    assert!(source_supports_contract(DOTA_INFO_COG));
}

#[test]
fn test_info_cog_has_help_command() {
    assert!(INFO_COG.commands.contains(&"help"));
}

#[test]
fn test_info_cog_has_leaderboard_command() {
    assert!(INFO_COG.commands.contains(&"leaderboard"));
}

#[test]
fn test_profile_cog_has_profile_command() {
    assert_eq!(PROFILE_COG.commands, ["profile"]);
}

#[test]
fn test_dota_info_cog_has_hero_and_ability_commands() {
    assert!(DOTA_INFO_COG.commands.contains(&"hero"));
    assert!(DOTA_INFO_COG.commands.contains(&"ability"));
}

#[test]
fn test_info_cog_can_be_instantiated() {
    let cog = instantiate_cog(INFO_COG, 123);
    assert_eq!(cog.bot_id, 123);
    assert_eq!(cog.contract.class_name, "InfoCommands");
}

#[test]
fn test_info_cog_constructor_exposes_only_supported_dependencies() {
    assert_eq!(
        INFO_COG.required_dependencies,
        ["bot", "player_service", "match_service"]
    );
    assert_eq!(
        INFO_COG.optional_dependencies,
        [
            "flavor_text_service",
            "guild_config_service",
            "gambling_stats_service",
            "prediction_service",
            "bankruptcy_service",
        ]
    );
}

#[test]
fn test_profile_cog_can_be_instantiated() {
    let cog = instantiate_cog(PROFILE_COG, 456);
    assert_eq!(cog.bot_id, 456);
    assert_eq!(cog.contract.required_dependencies, ["bot"]);
}

#[test]
fn test_all_extensions_importable() {
    assert!(all_extensions_have_sources());
}
