//! Transport-independent boss policy shared by the regular and pinnacle duel
//! runtimes: pet assists, risk-tier payouts, base rewards, and phase
//! transition events.

pub const PET_TRAINING_CAP: i32 = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PetMood {
    Happy,
    Content,
    Hungry,
    Starving,
}

impl PetMood {
    #[must_use]
    pub const fn boss_assist_percent(self) -> i32 {
        match self {
            Self::Happy => 12,
            Self::Content => 10,
            Self::Hungry => 8,
            Self::Starving => 5,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PetStage {
    Baby,
    Adult,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PetSnapshot {
    pub name: String,
    pub species_id: String,
    pub species_name: String,
    pub training_xp: i32,
    pub stage: PetStage,
    pub mood: PetMood,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PetAssist {
    pub pet_name: String,
    pub species_id: String,
    pub species_name: String,
    pub bonus_percent: i32,
}

#[must_use]
pub fn pet_boss_assist(pet: &PetSnapshot) -> Option<PetAssist> {
    if pet.stage != PetStage::Adult || pet.training_xp < PET_TRAINING_CAP {
        return None;
    }
    Some(PetAssist {
        pet_name: pet.name.clone(),
        species_id: pet.species_id.clone(),
        species_name: pet.species_name.clone(),
        bonus_percent: pet.mood.boss_assist_percent(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RiskTier {
    Cautious,
    Bold,
    Reckless,
}

impl RiskTier {
    const fn payout_index(self) -> usize {
        match self {
            Self::Cautious => 0,
            Self::Bold => 1,
            Self::Reckless => 2,
        }
    }
}

#[must_use]
pub fn boss_payout_multiplier(depth: i32, risk: RiskTier) -> f64 {
    let row = match depth {
        25 => [1.5, 2.4, 4.3],
        50 => [1.8, 3.3, 5.8],
        75 => [2.1, 4.3, 7.3],
        100 => [2.4, 4.8, 8.4],
        150 => [2.7, 5.2, 8.8],
        200 => [3.0, 5.8, 9.7],
        275 => [3.3, 6.5, 10.3],
        _ => [2.0, 3.0, 6.0],
    };
    row[risk.payout_index()]
}

#[must_use]
pub fn boss_base_reward(depth: i32, prestige: i32) -> i64 {
    let base = match depth {
        25 => 15,
        50 => 20,
        75 => 25,
        100 => 30,
        150 => 40,
        200 => 50,
        275 => 65,
        _ => 15,
    };
    let ascension_multiplier = if prestige >= 4 { 1.5 } else { 1.0 };
    (f64::from(base) * ascension_multiplier) as i64
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhaseTransitionEvent {
    pub id: &'static str,
    pub player_hp_delta: i32,
    pub boss_hp_delta: i32,
    pub player_hit_offset: f64,
    pub boss_hit_offset: f64,
    pub player_damage_delta: i32,
    pub boss_damage_delta: i32,
    pub luminosity_delta: i32,
}

pub const PHASE_TRANSITION_EVENTS: [PhaseTransitionEvent; 14] = [
    PhaseTransitionEvent {
        id: "cave_in",
        player_hp_delta: -1,
        boss_hp_delta: -1,
        player_hit_offset: 0.0,
        boss_hit_offset: 0.0,
        player_damage_delta: 0,
        boss_damage_delta: 0,
        luminosity_delta: 0,
    },
    PhaseTransitionEvent {
        id: "fissure",
        player_hp_delta: 0,
        boss_hp_delta: 0,
        player_hit_offset: -0.05,
        boss_hit_offset: 0.0,
        player_damage_delta: 0,
        boss_damage_delta: 0,
        luminosity_delta: -5,
    },
    PhaseTransitionEvent {
        id: "glowburst",
        player_hp_delta: 0,
        boss_hp_delta: 0,
        player_hit_offset: 0.0,
        boss_hit_offset: 0.05,
        player_damage_delta: 0,
        boss_damage_delta: 0,
        luminosity_delta: 20,
    },
    PhaseTransitionEvent {
        id: "cold_snap",
        player_hp_delta: 0,
        boss_hp_delta: 0,
        player_hit_offset: 0.0,
        boss_hit_offset: 0.0,
        player_damage_delta: -1,
        boss_damage_delta: -1,
        luminosity_delta: 0,
    },
    PhaseTransitionEvent {
        id: "spore_cloud",
        player_hp_delta: 0,
        boss_hp_delta: 0,
        player_hit_offset: -0.03,
        boss_hit_offset: -0.03,
        player_damage_delta: 0,
        boss_damage_delta: 0,
        luminosity_delta: 0,
    },
    PhaseTransitionEvent {
        id: "void_pull",
        player_hp_delta: -2,
        boss_hp_delta: -2,
        player_hit_offset: 0.0,
        boss_hit_offset: 0.0,
        player_damage_delta: 0,
        boss_damage_delta: 0,
        luminosity_delta: 0,
    },
    PhaseTransitionEvent {
        id: "echoing_drip",
        player_hp_delta: 0,
        boss_hp_delta: 0,
        player_hit_offset: 0.0,
        boss_hit_offset: 0.0,
        player_damage_delta: 0,
        boss_damage_delta: 0,
        luminosity_delta: -3,
    },
    PhaseTransitionEvent {
        id: "iron_tang",
        player_hp_delta: 0,
        boss_hp_delta: 0,
        player_hit_offset: 0.0,
        boss_hit_offset: 0.04,
        player_damage_delta: 0,
        boss_damage_delta: 0,
        luminosity_delta: 0,
    },
    PhaseTransitionEvent {
        id: "shifting_walls",
        player_hp_delta: -2,
        boss_hp_delta: 0,
        player_hit_offset: 0.0,
        boss_hit_offset: 0.0,
        player_damage_delta: 0,
        boss_damage_delta: 0,
        luminosity_delta: 0,
    },
    PhaseTransitionEvent {
        id: "hollow_tone",
        player_hp_delta: 0,
        boss_hp_delta: 0,
        player_hit_offset: -0.02,
        boss_hit_offset: 0.02,
        player_damage_delta: 0,
        boss_damage_delta: 0,
        luminosity_delta: 0,
    },
    PhaseTransitionEvent {
        id: "crystal_bloom",
        player_hp_delta: 0,
        boss_hp_delta: 0,
        player_hit_offset: 0.0,
        boss_hit_offset: 0.0,
        player_damage_delta: 1,
        boss_damage_delta: 0,
        luminosity_delta: 10,
    },
    PhaseTransitionEvent {
        id: "tremor",
        player_hp_delta: 0,
        boss_hp_delta: 0,
        player_hit_offset: 0.0,
        boss_hit_offset: 0.0,
        player_damage_delta: -1,
        boss_damage_delta: -1,
        luminosity_delta: 0,
    },
    PhaseTransitionEvent {
        id: "ash_fall",
        player_hp_delta: 0,
        boss_hp_delta: 0,
        player_hit_offset: -0.02,
        boss_hit_offset: 0.0,
        player_damage_delta: 0,
        boss_damage_delta: 0,
        luminosity_delta: -5,
    },
    PhaseTransitionEvent {
        id: "quiet",
        player_hp_delta: 0,
        boss_hp_delta: 0,
        player_hit_offset: 0.0,
        boss_hit_offset: 0.0,
        player_damage_delta: 0,
        boss_damage_delta: 0,
        luminosity_delta: 0,
    },
];

#[must_use]
pub fn transition_event(event_id: &str) -> Option<PhaseTransitionEvent> {
    PHASE_TRANSITION_EVENTS
        .iter()
        .find(|event| event.id == event_id)
        .copied()
}

#[cfg(test)]
#[path = "boss_duel/tests.rs"]
mod tests;
