//! Transport-independent boss encounter presentation.
//!
//! The runtime renders boss encounters and results; this module owns the
//! stable pinnacle secret-phase presentation roll and the pet-assist copy.

use crate::boss_duel::PetAssist;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BossInfo {
    pub boss_id: String,
    pub name: String,
    pub dialogue: String,
    pub is_pinnacle: bool,
    pub phase: u8,
    pub wager_allowed: bool,
    pub encounter_key: String,
    pub carried_wager: Option<i64>,
}

impl Default for BossInfo {
    fn default() -> Self {
        Self {
            boss_id: "test_boss".to_owned(),
            name: "Test Boss".to_owned(),
            dialogue: "The tunnel shakes.".to_owned(),
            is_pinnacle: false,
            phase: 1,
            wager_allowed: true,
            encounter_key: "encounter-1".to_owned(),
            carried_wager: None,
        }
    }
}

fn pet_flavor(species_id: &str) -> &'static str {
    match species_id {
        "common_cama" => "barrels into the fray",
        "dromedary_cross" => "leans in with a stubborn shoulder-check",
        "banana_ears" => "hums a battle note and charges",
        "embergear_cama" => "builds steam and rockets forward",
        "jopacama" => "finds a suspiciously profitable weak spot",
        "pudge_cama" => "takes a bite out of the opposition",
        "courier_cama" => "delivers a hit straight to the boss",
        "riverglow_cama" => "skates through the dark and clips the boss",
        "aegis_cama" => "slams its glowing shell into the boss",
        "invoker_cama" => "assembles several extremely learned sparks",
        "crystal_cama" => "freezes the boss's footing",
        "prismwool_cama" => "refracts the torchlight into a sharp flash",
        "rama" => "spits with historic contempt",
        "moondrift_cama" => "drifts through the boss's guard",
        "sunspun_cama" => "flares gold and strikes",
        _ => "lunges at the boss",
    }
}

/// Python's `_format_pet_assist`: the species flavor and assist percentage,
/// plus the damage outcome once the fight has resolved.
#[must_use]
pub fn format_pet_assist(assist: &PetAssist, bonus_damage: Option<i32>) -> String {
    let mut line = format!(
        "**{}** ({}) {}, lending a **{}%** damage assist.",
        assist.pet_name,
        assist.species_name,
        pet_flavor(&assist.species_id),
        assist.bonus_percent
    );
    if let Some(damage) = bonus_damage {
        if damage > 0 {
            line.push_str(&format!("\nIt contributed **{damage} bonus damage**."));
        } else {
            line.push_str("\nNo opening appeared this time.");
        }
    }
    line
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecretPhaseDefinition {
    pub title: String,
    pub dialogue: Vec<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EncounterPresentation {
    pub name: String,
    pub dialogue: String,
    pub secret: bool,
}

fn stable_unit_interval(seed: &str) -> f64 {
    // FNV-1a is deliberately specified here instead of relying on a process-
    // randomized hasher. The same encounter key therefore never re-rolls.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in seed.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Avalanche nearby encounter timestamps; raw FNV values share high bits
    // for long common prefixes and would otherwise cluster at a 50% cutoff.
    hash ^= hash >> 30;
    hash = hash.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    hash ^= hash >> 27;
    hash = hash.wrapping_mul(0x94d0_49bb_1331_11eb);
    hash ^= hash >> 31;
    (hash as f64) / (u64::MAX as f64)
}

#[must_use]
pub fn resolve_encounter_presentation(
    info: &BossInfo,
    phase: u8,
    seed_key: &str,
    secret_chance: f64,
    secret_definition: Option<&SecretPhaseDefinition>,
    forced_roll: Option<f64>,
) -> EncounterPresentation {
    let normal = || EncounterPresentation {
        name: info.name.clone(),
        dialogue: info.dialogue.clone(),
        secret: false,
    };
    if phase != 3 || !info.is_pinnacle || info.boss_id.is_empty() {
        return normal();
    }
    let Some(secret) = secret_definition else {
        return normal();
    };
    if forced_roll.unwrap_or_else(|| stable_unit_interval(seed_key)) >= secret_chance {
        return normal();
    }
    let dialogue = if secret.dialogue.is_empty() {
        info.dialogue.clone()
    } else {
        let choice_seed = format!("{seed_key}:dialogue");
        let index = (stable_unit_interval(&choice_seed) * secret.dialogue.len() as f64) as usize;
        secret.dialogue[index.min(secret.dialogue.len() - 1)].clone()
    };
    EncounterPresentation {
        name: secret.title.clone(),
        dialogue,
        secret: true,
    }
}

#[cfg(test)]
#[path = "boss_encounter_view_guard/tests.rs"]
mod tests;
