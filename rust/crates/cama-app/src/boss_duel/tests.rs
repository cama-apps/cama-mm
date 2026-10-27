use super::*;

fn pet(mood: PetMood, stage: PetStage, training_xp: i32) -> PetSnapshot {
    PetSnapshot {
        name: "Dane".to_owned(),
        species_id: "common_cama".to_owned(),
        species_name: "Common Cama".to_owned(),
        training_xp,
        stage,
        mood,
    }
}

fn assist(mood: PetMood) -> PetAssist {
    pet_boss_assist(&pet(mood, PetStage::Adult, PET_TRAINING_CAP)).expect("eligible pet")
}

#[test]
fn test_fully_trained_adult_pet_assist_scales_with_mood_happy_12() {
    assert_eq!(assist(PetMood::Happy).bonus_percent, 12);
}

#[test]
fn test_fully_trained_adult_pet_assist_scales_with_mood_content_10() {
    assert_eq!(assist(PetMood::Content).bonus_percent, 10);
}

#[test]
fn test_fully_trained_adult_pet_assist_scales_with_mood_hungry_8() {
    assert_eq!(assist(PetMood::Hungry).bonus_percent, 8);
}

#[test]
fn test_fully_trained_adult_pet_assist_scales_with_mood_starving_5() {
    assert_eq!(assist(PetMood::Starving).bonus_percent, 5);
}

#[test]
fn test_pet_boss_assist_requires_adult_and_training_cap_baby_20() {
    assert_eq!(
        pet_boss_assist(&pet(PetMood::Happy, PetStage::Baby, PET_TRAINING_CAP)),
        None
    );
}

#[test]
fn test_pet_boss_assist_requires_adult_and_training_cap_adult_19() {
    assert_eq!(
        pet_boss_assist(&pet(PetMood::Happy, PetStage::Adult, PET_TRAINING_CAP - 1,)),
        None
    );
}

#[test]
fn test_p4_ascension_scales_regular_boss_base_reward() {
    assert_eq!(boss_base_reward(25, 3), 15);
    assert_eq!(boss_base_reward(25, 4), 22);
    assert_eq!(boss_base_reward(275, 4), 97);
}
