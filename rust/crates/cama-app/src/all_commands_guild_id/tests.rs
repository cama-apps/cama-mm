use super::*;

fn assert_no_guild_id_issue(filename: &str) {
    let command = command_source_by_filename(filename).expect("embedded command source");
    let issues = find_guild_id_issues(filename, command.source);
    assert!(issues.is_empty(), "{filename}: {issues:#?}");
}

#[test]
fn test_guild_id_defined_before_use_registration_py() {
    assert_no_guild_id_issue("registration.py");
}

#[test]
fn test_guild_id_defined_before_use_dota_info_py() {
    assert_no_guild_id_issue("dota_info.py");
}

#[test]
fn test_guild_id_defined_before_use_tax_py() {
    assert_no_guild_id_issue("tax.py");
}

#[test]
fn test_guild_id_defined_before_use_enrichment_py() {
    assert_no_guild_id_issue("enrichment.py");
}

#[test]
fn test_guild_id_defined_before_use_survey_py() {
    assert_no_guild_id_issue("survey.py");
}

#[test]
fn test_guild_id_defined_before_use_predictions_py() {
    assert_no_guild_id_issue("predictions.py");
}

#[test]
fn test_guild_id_defined_before_use_pet_py() {
    assert_no_guild_id_issue("pet.py");
}

#[test]
fn test_guild_id_defined_before_use_wrapped_py() {
    assert_no_guild_id_issue("wrapped.py");
}

#[test]
fn test_guild_id_defined_before_use_ask_py() {
    assert_no_guild_id_issue("ask.py");
}

#[test]
fn test_guild_id_defined_before_use_blame_luke_py() {
    assert_no_guild_id_issue("blame_luke.py");
}

#[test]
fn test_guild_id_defined_before_use_checks_py() {
    assert_no_guild_id_issue("checks.py");
}

#[test]
fn test_guild_id_defined_before_use_duel_py() {
    assert_no_guild_id_issue("duel.py");
}

#[test]
fn test_guild_id_defined_before_use_profile_py() {
    assert_no_guild_id_issue("profile.py");
}

#[test]
fn test_guild_id_defined_before_use_rating_analysis_py() {
    assert_no_guild_id_issue("rating_analysis.py");
}

#[test]
fn test_guild_id_defined_before_use_init_py() {
    assert_no_guild_id_issue("__init__.py");
}

#[test]
fn test_guild_id_defined_before_use_reminders_py() {
    assert_no_guild_id_issue("reminders.py");
}

#[test]
fn test_guild_id_defined_before_use_scout_py() {
    assert_no_guild_id_issue("scout.py");
}

#[test]
fn test_guild_id_defined_before_use_betting_py() {
    assert_no_guild_id_issue("betting.py");
}

#[test]
fn test_guild_id_defined_before_use_advstats_py() {
    assert_no_guild_id_issue("advstats.py");
}

#[test]
fn test_guild_id_defined_before_use_dig_py() {
    assert_no_guild_id_issue("dig.py");
}

#[test]
fn test_guild_id_defined_before_use_admin_py() {
    assert_no_guild_id_issue("admin.py");
}

#[test]
fn test_guild_id_defined_before_use_mana_py() {
    assert_no_guild_id_issue("mana.py");
}

#[test]
fn test_guild_id_defined_before_use_trivia_py() {
    assert_no_guild_id_issue("trivia.py");
}

#[test]
fn test_guild_id_defined_before_use_herogrid_py() {
    assert_no_guild_id_issue("herogrid.py");
}

#[test]
fn test_guild_id_defined_before_use_info_py() {
    assert_no_guild_id_issue("info.py");
}

#[test]
fn test_guild_id_defined_before_use_lobby_py() {
    assert_no_guild_id_issue("lobby.py");
}

#[test]
fn test_guild_id_defined_before_use_shop_py() {
    assert_no_guild_id_issue("shop.py");
}

#[test]
fn test_guild_id_defined_before_use_player_trivia_py() {
    assert_no_guild_id_issue("player_trivia.py");
}

#[test]
fn test_guild_id_defined_before_use_mafia_py() {
    assert_no_guild_id_issue("mafia.py");
}

#[test]
fn test_guild_id_defined_before_use_draft_py() {
    assert_no_guild_id_issue("draft.py");
}

#[test]
fn test_guild_id_defined_before_use_match_py() {
    assert_no_guild_id_issue("match.py");
}

#[test]
fn test_all_command_files_can_be_imported() {
    validate_maintained_command_catalog().expect("maintained command source catalog");
    assert_eq!(MAINTAINED_COMMAND_MODULES.len(), 15);

    // Prove this is a behavioral analyzer rather than a filename allowlist.
    let bad = "async def command():\n    service.get(1, guild_id)\n    guild_id = 9\n";
    let issues = find_guild_id_issues("broken.py", bad);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].line, 2);
    assert!(issues[0].description.contains("before defined on line 3"));

    let safe = "async def command():\n    guild_id = 9\n    service.get(1, guild_id)\n";
    assert!(find_guild_id_issues("safe.py", safe).is_empty());
}
