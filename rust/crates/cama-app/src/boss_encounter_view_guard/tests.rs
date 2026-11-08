use std::collections::BTreeSet;

use super::*;

fn pinnacle_info() -> BossInfo {
    BossInfo {
        boss_id: "forgotten_king".to_owned(),
        name: "The Last Breath of Kings".to_owned(),
        dialogue: "Last breath.".to_owned(),
        is_pinnacle: true,
        phase: 3,
        ..BossInfo::default()
    }
}

fn secret_definition() -> SecretPhaseDefinition {
    SecretPhaseDefinition {
        title: "The King Behind the Crown".to_owned(),
        dialogue: vec![
            "The crown remembers.".to_owned(),
            "No court remains.".to_owned(),
        ],
    }
}

#[test]
fn test_phase3_secret_roll_changes_only_presentation_secret() {
    let info = pinnacle_info();
    let display = resolve_encounter_presentation(
        &info,
        3,
        "42:forgotten_king:3:one",
        0.10,
        Some(&secret_definition()),
        Some(0.05),
    );
    assert_eq!(display.name, "The King Behind the Crown");
    assert!(secret_definition().dialogue.contains(&display.dialogue));
    assert!(display.secret);
    assert_eq!(info.name, "The Last Breath of Kings");
    assert_eq!(info.dialogue, "Last breath.");
}

#[test]
fn test_phase3_secret_roll_changes_only_presentation_normal() {
    let info = pinnacle_info();
    let display = resolve_encounter_presentation(
        &info,
        3,
        "42:forgotten_king:3:one",
        0.10,
        Some(&secret_definition()),
        Some(0.50),
    );
    assert_eq!(display.name, "The Last Breath of Kings");
    assert_eq!(display.dialogue, "Last breath.");
    assert!(!display.secret);
    assert_eq!(info.name, "The Last Breath of Kings");
}

#[test]
fn test_pinnacle_secret_phase_is_stable_for_one_encounter() {
    let info = pinnacle_info();
    let secret = secret_definition();
    let renders = (0..12)
        .map(|_| {
            resolve_encounter_presentation(
                &info,
                3,
                "4242:forgotten_king:3:1000000",
                0.5,
                Some(&secret),
                None,
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(renders.len(), 1);
    let per_player = (0..40)
        .map(|user| {
            resolve_encounter_presentation(
                &info,
                3,
                &format!("{user}:forgotten_king:3:1000000"),
                0.5,
                Some(&secret),
                None,
            )
            .secret
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(per_player, BTreeSet::from([false, true]));
    let per_encounter = (1_000_000..1_000_060)
        .map(|engaged_at| {
            resolve_encounter_presentation(
                &info,
                3,
                &format!("4242:forgotten_king:3:{engaged_at}"),
                0.5,
                Some(&secret),
                None,
            )
            .secret
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(per_encounter, BTreeSet::from([false, true]));
}

fn pet(species_id: &str, pet_name: &str, species_name: &str, bonus_percent: i32) -> PetAssist {
    PetAssist {
        pet_name: pet_name.to_owned(),
        species_id: species_id.to_owned(),
        species_name: species_name.to_owned(),
        bonus_percent,
    }
}

#[test]
fn test_pet_assist_reports_species_flavor_and_pet_damage() {
    let line = format_pet_assist(&pet("rama", "Rama", "Rama the First", 10), Some(2));
    assert_eq!(
        line,
        "**Rama** (Rama the First) spits with historic contempt, lending a **10%** damage assist.\nIt contributed **2 bonus damage**."
    );
}

#[test]
fn test_pet_assist_reports_when_pet_found_no_opening() {
    let line = format_pet_assist(&pet("common_cama", "Dane", "Common Cama", 12), Some(0));
    assert!(line.contains("barrels into the fray"));
    assert!(line.ends_with("\nNo opening appeared this time."));
}

#[test]
fn test_pet_assist_without_outcome_and_unknown_species() {
    assert_eq!(
        format_pet_assist(&pet("mystery", "Nib", "Odd Cama", 5), None),
        "**Nib** (Odd Cama) lunges at the boss, lending a **5%** damage assist."
    );
}
