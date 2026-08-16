use super::*;
use crate::mafia_service::{GameStatus, Phase};

fn game(twist: Option<Twist>) -> Game {
    Game {
        game_id: 1,
        guild_id: 42,
        game_date: "2026-04-24".to_owned(),
        phase: Phase::Night,
        roster_size: 8,
        twist,
        day_number: 1,
        winner: None,
        entry_fee: 8,
        status: GameStatus::Active,
    }
}

fn victim(role: Role) -> Player {
    Player {
        discord_id: 999,
        guild_id: 42,
        role,
        is_godfather: false,
        hero_name: "Pudge".to_owned(),
        is_alive: true,
        eliminated_phase: None,
    }
}

#[test]
fn test_setup_narration_returns_string() {
    assert!(
        !MafiaFlavorService::new(0)
            .setup_narration(&game(None))
            .is_empty()
    );
}

#[test]
fn test_setup_includes_twist_line() {
    let text = MafiaFlavorService::new(0).setup_narration(&game(Some(Twist::BloodMoon)));
    assert!(text.to_lowercase().contains("blood moon"));
}

#[test]
fn test_death_narration_includes_role_and_hero() {
    let text = MafiaFlavorService::new(0).death_narration(&victim(Role::Townie), false);
    assert!(text.contains("Pudge"));
    assert!(text.contains("Townie"));
    assert!(text.contains("<@999>"));
}

#[test]
fn test_plague_death_uses_plague_template() {
    let text = MafiaFlavorService::new(0).death_narration(&victim(Role::Townie), true);
    assert!(text.to_lowercase().contains("plague") || text.to_lowercase().contains("fever"));
}

#[test]
fn test_lynch_narration_includes_role() {
    assert!(
        MafiaFlavorService::new(0)
            .lynch_narration(&victim(Role::Mafia))
            .contains("Mafia")
    );
}

#[test]
fn test_resolution_narrations_per_winner() {
    let service = MafiaFlavorService::new(0);
    for winner in [Winner::Town, Winner::Mafia, Winner::Jester, Winner::None] {
        assert!(!service.resolution_narration(winner).is_empty());
    }
}

#[test]
fn test_no_lynch_narration() {
    assert!(!MafiaFlavorService::new(0).no_lynch_narration().is_empty());
}

#[test]
fn all_templates_render_without_unexpanded_player_tokens() {
    let service = MafiaFlavorService::new(0);
    let victim = victim(Role::Bookie);
    for _ in 0..DEATH.len() * 2 {
        let rendered = service.death_narration(&victim, false);
        assert!(!rendered.contains("{hero}"));
        assert!(!rendered.contains("{name}"));
        assert!(!rendered.contains("{role}"));
    }
}
