//! Pure combat rules for pet brawls.
//!
//! A [`Duelist`] is an immutable snapshot of persisted pet state. Combat is
//! deterministic for an injected [`BrawlRng`], performs no I/O, and never
//! mutates the pet record from which that snapshot was built.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use crate::pet::{PetStage, SpeciesTier, get_species};

pub const MAX_ROUNDS: u8 = 8;

pub const BASE_HP: i32 = 80;
pub const ADULT_HP_BONUS: i32 = 20;
pub const HUNGER_HP_PENALTY_DIV: i32 = 5;

pub const SPIT_DMG: (i32, i32) = (8, 16);
pub const STAMPEDE_DMG: (i32, i32) = (16, 32);
pub const STAMPEDE_MISS_PCT: i32 = 55;
pub const STAMPEDE_FEINT_MISS_PCT: i32 = 40;
pub const FEINT_DMG: (i32, i32) = (8, 14);
pub const FEINT_INTERRUPTED_DAMAGE_PCT: i32 = 50;
pub const SIDESTEP_COUNTER_DMG: (i32, i32) = (6, 10);
pub const SIDESTEP_DAMAGE_TAKEN_PCT: i32 = 25;
pub const HUNKER_HEAL: (i32, i32) = (5, 8);
pub const HUNKER_DAMAGE_TAKEN_PCT: i32 = 30;
pub const HUNKER_STAMPEDE_DAMAGE_TAKEN_PCT: i32 = 60;
pub const BASE_CRIT_PCT: i32 = 10;
pub const SPITTER_CRIT_PCT: i32 = 25;
pub const CHILL_MISS_BONUS_PP: i32 = 10;
pub const MOONDRIFT_MISS_BONUS_PP: i32 = 15;
pub const RAVENOUS_HEAL_BONUS: i32 = 3;
pub const SUNSPUN_HEAL_BONUS: i32 = 5;
pub const TRAINING_STR_DAMAGE: i32 = 1;
pub const TRAINING_STR_STAMPEDE_BONUS: i32 = 1;
pub const TRAINING_DEX_CRIT_PP: i32 = 2;
pub const TRAINING_DEX_SPIT_CRIT_BONUS_PP: i32 = 2;
pub const TRAINING_DEX_DODGE_PP: i32 = 2;
pub const TRAINING_INT_MITIGATION: i32 = 1;
pub const TRAINING_INT_HUNKER_BONUS: i32 = 1;

pub const SHELL_BLURB: &str = "shelled — survives the first knockout on 1 HP";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PetBrawlMove {
    Spit,
    Stampede,
    Hunker,
    Feint,
    Sidestep,
}

impl PetBrawlMove {
    pub const ALL: [Self; 5] = [
        Self::Spit,
        Self::Stampede,
        Self::Hunker,
        Self::Feint,
        Self::Sidestep,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spit => "spit",
            Self::Stampede => "stampede",
            Self::Hunker => "hunker",
            Self::Feint => "feint",
            Self::Sidestep => "sidestep",
        }
    }

    #[must_use]
    pub const fn emoji(self) -> &'static str {
        match self {
            Self::Spit => "💦",
            Self::Stampede => "🐫",
            Self::Hunker => "🛡️",
            Self::Feint => "🎭",
            Self::Sidestep => "🌀",
        }
    }

    #[must_use]
    pub const fn blurb(self) -> &'static str {
        match self {
            Self::Spit => "reliable chip that never misses; pressures Stampede and Feint.",
            Self::Stampede => concat!(
                "big damage; 55% whiff normally, 40% into Feint, ",
                "never into Hunker."
            ),
            Self::Hunker => concat!(
                "soak Spit and recover while waiting out Sidestep; Stampede and ",
                "Feint break the guard."
            ),
            Self::Feint => "bait Hunker and Sidestep; landed Spit or Stampede halves the trick.",
            Self::Sidestep => concat!(
                "take 25% from landed Spit or Stampede and counter; Hunker and Feint ",
                "deny the opening."
            ),
        }
    }

    #[must_use]
    const fn default_name(self) -> &'static str {
        match self {
            Self::Spit => "Spit",
            Self::Stampede => "Stampede",
            Self::Hunker => "Hunker Down",
            Self::Feint => "Feint",
            Self::Sidestep => "Sidestep",
        }
    }

    #[must_use]
    const fn is_direct_attack(self) -> bool {
        matches!(self, Self::Spit | Self::Stampede)
    }
}

#[must_use]
pub fn move_name(species_id: &str, move_: PetBrawlMove) -> &'static str {
    use PetBrawlMove::{Feint, Hunker, Sidestep, Spit, Stampede};
    match (species_id, move_) {
        ("common_cama", Spit) => "Unremarkable Spit",
        ("common_cama", Stampede) => "Ordinary Stampede",
        ("common_cama", Hunker) => "Stand There",
        ("common_cama", Feint) => "Obvious Feint",
        ("common_cama", Sidestep) => "Awkward Sidestep",
        ("dromedary_cross", Spit) => "Sandy Spit",
        ("dromedary_cross", Stampede) => "Dune Charge",
        ("dromedary_cross", Hunker) => "Close Nostrils",
        ("dromedary_cross", Feint) => "Mirage Feint",
        ("dromedary_cross", Sidestep) => "Dune Drift",
        ("banana_ears", Spit) => "Melodic Spit",
        ("banana_ears", Stampede) => "Humming Charge",
        ("banana_ears", Hunker) => "Hum Defensively",
        ("banana_ears", Feint) => "False Note",
        ("banana_ears", Sidestep) => "Rhythm Step",
        ("embergear_cama", Spit) => "Spark Spit",
        ("embergear_cama", Stampede) => "Overdrive",
        ("embergear_cama", Hunker) => "Vent Heat",
        ("embergear_cama", Feint) => "False Start",
        ("embergear_cama", Sidestep) => "Quickshift",
        ("jopacama", Spit) => "Coin-Flecked Spit",
        ("jopacama", Stampede) => "Market Crash",
        ("jopacama", Hunker) => "Audit Defense",
        ("jopacama", Feint) => "Market Bluff",
        ("jopacama", Sidestep) => "Liquidity Exit",
        ("pudge_cama", Spit) => "Sloppy Spit",
        ("pudge_cama", Stampede) => "Dinner Rush",
        ("pudge_cama", Hunker) => "Digest",
        ("pudge_cama", Feint) => "Fake Dinner",
        ("pudge_cama", Sidestep) => "Greasy Shuffle",
        ("courier_cama", Spit) => "Special Delivery",
        ("courier_cama", Stampede) => "Rush Delivery",
        ("courier_cama", Hunker) => "Hide Behind Saddlebags",
        ("courier_cama", Feint) => "Decoy Delivery",
        ("courier_cama", Sidestep) => "Express Detour",
        ("riverglow_cama", Spit) => "Charged Spit",
        ("riverglow_cama", Stampede) => "River Rush",
        ("riverglow_cama", Hunker) => "Bottle Up",
        ("riverglow_cama", Feint) => "False Current",
        ("riverglow_cama", Sidestep) => "Slipstream",
        ("aegis_cama", Spit) => "Serene Spit",
        ("aegis_cama", Stampede) => "Shell Rush",
        ("aegis_cama", Hunker) => "Shell Up",
        ("aegis_cama", Feint) => "Shell Game",
        ("aegis_cama", Sidestep) => "Shield Step",
        ("invoker_cama", Spit) => "Forged Spit",
        ("invoker_cama", Stampede) => "Orb-Powered Charge",
        ("invoker_cama", Hunker) => "Quas Ward",
        ("invoker_cama", Feint) => "Wex Feint",
        ("invoker_cama", Sidestep) => "Ghost Walk",
        ("crystal_cama", Spit) => "Frost Spit",
        ("crystal_cama", Stampede) => "Blizzard Stampede",
        ("crystal_cama", Hunker) => "Frozen Stance",
        ("crystal_cama", Feint) => "Ice Decoy",
        ("crystal_cama", Sidestep) => "Glacial Step",
        ("prismwool_cama", Spit) => "Prismatic Spit",
        ("prismwool_cama", Stampede) => "Linked Charge",
        ("prismwool_cama", Hunker) => "Wool Ward",
        ("prismwool_cama", Feint) => "Refracted Feint",
        ("prismwool_cama", Sidestep) => "Spectrum Shift",
        ("rama", Spit) => "Royal Spit",
        ("rama", Stampede) => "Temperamental Rampage",
        ("rama", Hunker) => "Refuse to Cooperate",
        ("rama", Feint) => "Royal Bluff",
        ("rama", Sidestep) => "Majestic Exit",
        ("moondrift_cama", Spit) => "Moonlit Spit",
        ("moondrift_cama", Stampede) => "Tidal Rush",
        ("moondrift_cama", Hunker) => "Eclipse",
        ("moondrift_cama", Feint) => "Moonshadow",
        ("moondrift_cama", Sidestep) => "Tidal Step",
        ("sunspun_cama", Spit) => "Solar Flare",
        ("sunspun_cama", Stampede) => "Dawn Charge",
        ("sunspun_cama", Hunker) => "Corona Guard",
        ("sunspun_cama", Feint) => "Dawn Mirage",
        ("sunspun_cama", Sidestep) => "Sunbeam Step",
        _ => move_.default_name(),
    }
}

/// Whether the species has an explicit flavor entry for a move.
#[must_use]
pub fn has_move_flavor(species_id: &str, move_: PetBrawlMove) -> bool {
    crate::pet::SPECIES
        .iter()
        .any(|species| species.species_id == species_id)
        && move_name(species_id, move_) != move_.default_name()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrawlTraits {
    pub dodge_bonus_pp: i32,
    pub crit_pct: i32,
    pub damage_dealt_bonus: i32,
    pub damage_taken_mod: i32,
    pub heal_bonus: i32,
    pub loss_insurance: bool,
}

impl BrawlTraits {
    pub const NONE: Self = Self {
        dodge_bonus_pp: 0,
        crit_pct: BASE_CRIT_PCT,
        damage_dealt_bonus: 0,
        damage_taken_mod: 0,
        heal_bonus: 0,
        loss_insurance: false,
    };
}

impl Default for BrawlTraits {
    fn default() -> Self {
        Self::NONE
    }
}

/// Species covered by the explicit combat-trait catalog. Quirkless entries
/// are deliberately retained so adding a species cannot silently omit its
/// combat-design decision.
pub const BRAWL_TRAIT_SPECIES: [&str; 15] = [
    "common_cama",
    "dromedary_cross",
    "banana_ears",
    "embergear_cama",
    "jopacama",
    "pudge_cama",
    "courier_cama",
    "riverglow_cama",
    "aegis_cama",
    "invoker_cama",
    "crystal_cama",
    "prismwool_cama",
    "rama",
    "moondrift_cama",
    "sunspun_cama",
];

#[must_use]
pub fn brawl_traits(species_id: &str) -> BrawlTraits {
    match species_id {
        "dromedary_cross" => BrawlTraits {
            damage_taken_mod: -1,
            ..BrawlTraits::NONE
        },
        "jopacama" => BrawlTraits {
            loss_insurance: true,
            ..BrawlTraits::NONE
        },
        "pudge_cama" => BrawlTraits {
            damage_dealt_bonus: 1,
            damage_taken_mod: 1,
            heal_bonus: RAVENOUS_HEAL_BONUS,
            ..BrawlTraits::NONE
        },
        "crystal_cama" => BrawlTraits {
            dodge_bonus_pp: CHILL_MISS_BONUS_PP,
            ..BrawlTraits::NONE
        },
        "rama" => BrawlTraits {
            crit_pct: SPITTER_CRIT_PCT,
            ..BrawlTraits::NONE
        },
        "moondrift_cama" => BrawlTraits {
            dodge_bonus_pp: MOONDRIFT_MISS_BONUS_PP,
            ..BrawlTraits::NONE
        },
        "sunspun_cama" => BrawlTraits {
            heal_bonus: SUNSPUN_HEAL_BONUS,
            ..BrawlTraits::NONE
        },
        _ => BrawlTraits::NONE,
    }
}

#[must_use]
pub fn trait_blurb(species_id: &str) -> Option<&'static str> {
    match species_id {
        "moondrift_cama" => Some("elusive — enemy Stampedes whiff far more often"),
        "sunspun_cama" => Some("radiant — recovers far more while hunkered"),
        "dromedary_cross" => Some("hardy — shrugs a point off every hit"),
        "jopacama" => Some("insured — loses less fullness in defeat"),
        "pudge_cama" => Some("ravenous — hits harder, takes more, heals extra hunkered"),
        "crystal_cama" => Some("chilling — enemy Stampedes whiff more often"),
        "rama" => Some("royal temper — crits far more often"),
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Duelist {
    pub pet_id: i64,
    pub owner_id: i64,
    pub name: String,
    pub species_id: String,
    pub tier: SpeciesTier,
    pub is_adult: bool,
    pub power: i32,
    pub training_str: i32,
    pub training_int: i32,
    pub training_dex: i32,
    pub max_hp: i32,
    pub hp: i32,
    pub shield_available: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrawlWinner {
    A,
    B,
    Draw,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PetBrawlState {
    pub a: Duelist,
    pub b: Duelist,
    pub round_no: u8,
    pub winner: Option<BrawlWinner>,
}

#[derive(Clone, Copy, Debug)]
pub struct DuelistSnapshot<'a> {
    pub pet_id: i64,
    pub owner_id: i64,
    pub name: &'a str,
    pub species_id: &'a str,
    pub stage: PetStage,
    pub hunger: i32,
    pub happy: bool,
    pub aegis_used: i64,
    pub training_str: i32,
    pub training_int: i32,
    pub training_dex: i32,
}

#[must_use]
pub fn build_duelist(snapshot: DuelistSnapshot<'_>) -> Duelist {
    let species = get_species(snapshot.species_id);
    let is_adult = snapshot.stage == PetStage::Adult;
    let max_hp = BASE_HP + if is_adult { ADULT_HP_BONUS } else { 0 };
    let hp = max_hp - (100 - snapshot.hunger.clamp(0, 100)) / HUNGER_HP_PENALTY_DIV;
    let tier_power = match species.tier {
        SpeciesTier::Common => 0,
        SpeciesTier::Uncommon | SpeciesTier::Rare => 1,
        SpeciesTier::Legendary => 2,
    };
    Duelist {
        pet_id: snapshot.pet_id,
        owner_id: snapshot.owner_id,
        name: snapshot.name.to_owned(),
        species_id: snapshot.species_id.to_owned(),
        tier: species.tier,
        is_adult,
        power: tier_power + i32::from(snapshot.happy),
        training_str: snapshot.training_str,
        training_int: snapshot.training_int,
        training_dex: snapshot.training_dex,
        max_hp,
        hp,
        shield_available: species.has_aegis && snapshot.aegis_used == 0,
    }
}

#[must_use]
pub const fn initial_state(a: Duelist, b: Duelist) -> PetBrawlState {
    PetBrawlState {
        a,
        b,
        round_no: 0,
        winner: None,
    }
}

/// Inclusive integer RNG used by the combat engine.
///
/// Implementations must consume one value per call and return a value within
/// the supplied bounds. This mirrors Python's injected `random.Random`
/// contract and makes call order directly scriptable in parity tests.
pub trait BrawlRng {
    fn randint(&mut self, low: i32, high: i32) -> i32;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolveRoundError {
    AlreadyResolved,
}

impl Display for ResolveRoundError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyResolved => formatter.write_str("brawl already resolved"),
        }
    }
}

impl Error for ResolveRoundError {}

#[derive(Clone, Copy, Debug)]
struct AttackResult {
    damage: i32,
    landed: bool,
    crit: bool,
}

fn ceil_percent(value: i32, percent: i32) -> i32 {
    (value * percent + 99) / 100
}

fn attack_damage<R: BrawlRng>(
    attacker: &Duelist,
    defender: &Duelist,
    move_: PetBrawlMove,
    defender_move: PetBrawlMove,
    rng: &mut R,
) -> AttackResult {
    let attacker_traits = brawl_traits(&attacker.species_id);
    let defender_traits = brawl_traits(&defender.species_id);
    if matches!(move_, PetBrawlMove::Hunker | PetBrawlMove::Sidestep) {
        return AttackResult {
            damage: 0,
            landed: false,
            crit: false,
        };
    }
    if move_ == PetBrawlMove::Stampede && defender_move != PetBrawlMove::Hunker {
        let base_miss_pct = if defender_move == PetBrawlMove::Feint {
            STAMPEDE_FEINT_MISS_PCT
        } else {
            STAMPEDE_MISS_PCT
        };
        let miss_pct = base_miss_pct
            + defender_traits.dodge_bonus_pp
            + defender.training_dex * TRAINING_DEX_DODGE_PP;
        if rng.randint(1, 100) <= miss_pct {
            return AttackResult {
                damage: 0,
                landed: false,
                crit: false,
            };
        }
    }
    let (low, high) = match move_ {
        PetBrawlMove::Stampede => STAMPEDE_DMG,
        PetBrawlMove::Feint => FEINT_DMG,
        PetBrawlMove::Spit => SPIT_DMG,
        PetBrawlMove::Hunker | PetBrawlMove::Sidestep => unreachable!(),
    };
    let mut strength_damage = attacker.training_str * TRAINING_STR_DAMAGE;
    if move_ == PetBrawlMove::Stampede {
        strength_damage += attacker.training_str * TRAINING_STR_STAMPEDE_BONUS;
    }
    let mut damage = rng.randint(low, high) + attacker.power + strength_damage;
    let mut crit_pct = attacker_traits.crit_pct + attacker.training_dex * TRAINING_DEX_CRIT_PP;
    if move_ == PetBrawlMove::Spit {
        crit_pct += attacker.training_dex * TRAINING_DEX_SPIT_CRIT_BONUS_PP;
    }
    let crit = rng.randint(1, 100) <= crit_pct;
    if crit {
        damage *= 2;
    }
    damage += attacker_traits.damage_dealt_bonus;
    if defender_move == PetBrawlMove::Hunker && move_ != PetBrawlMove::Feint {
        let percent = if move_ == PetBrawlMove::Stampede {
            HUNKER_STAMPEDE_DAMAGE_TAKEN_PCT
        } else {
            HUNKER_DAMAGE_TAKEN_PCT
        };
        damage = ceil_percent(damage, percent);
    } else if defender_move == PetBrawlMove::Sidestep && move_.is_direct_attack() {
        damage = ceil_percent(damage, SIDESTEP_DAMAGE_TAKEN_PCT);
    }
    damage += defender_traits.damage_taken_mod - defender.training_int * TRAINING_INT_MITIGATION;
    if defender_traits.damage_taken_mod < 0 || defender.training_int != 0 {
        damage = damage.max(1);
    }
    AttackResult {
        damage,
        landed: true,
        crit,
    }
}

fn sidestep_counter_damage<R: BrawlRng>(
    attacker: &Duelist,
    defender: &Duelist,
    rng: &mut R,
) -> (i32, bool) {
    let attacker_traits = brawl_traits(&attacker.species_id);
    let defender_traits = brawl_traits(&defender.species_id);
    let mut damage = rng.randint(SIDESTEP_COUNTER_DMG.0, SIDESTEP_COUNTER_DMG.1)
        + attacker.power
        + attacker.training_str * TRAINING_STR_DAMAGE;
    let crit_pct = attacker_traits.crit_pct + attacker.training_dex * TRAINING_DEX_CRIT_PP;
    let crit = rng.randint(1, 100) <= crit_pct;
    if crit {
        damage *= 2;
    }
    damage += attacker_traits.damage_dealt_bonus;
    damage += defender_traits.damage_taken_mod - defender.training_int * TRAINING_INT_MITIGATION;
    if defender_traits.damage_taken_mod < 0 || defender.training_int != 0 {
        damage = damage.max(1);
    }
    (damage, crit)
}

fn hunker_heal<R: BrawlRng>(hunkerer: &Duelist, rng: &mut R) -> i32 {
    rng.randint(HUNKER_HEAL.0, HUNKER_HEAL.1)
        + brawl_traits(&hunkerer.species_id).heal_bonus
        + hunkerer.training_int * TRAINING_INT_HUNKER_BONUS
}

pub fn resolve_round<R: BrawlRng>(
    state: &PetBrawlState,
    move_a: PetBrawlMove,
    move_b: PetBrawlMove,
    rng: &mut R,
) -> Result<(PetBrawlState, Vec<String>), ResolveRoundError> {
    if state.winner.is_some() {
        return Err(ResolveRoundError::AlreadyResolved);
    }

    let mut a_attack = attack_damage(&state.a, &state.b, move_a, move_b, rng);
    let mut b_attack = attack_damage(&state.b, &state.a, move_b, move_a, rng);
    if move_a == PetBrawlMove::Feint && move_b.is_direct_attack() && b_attack.landed {
        a_attack.damage = ceil_percent(a_attack.damage, FEINT_INTERRUPTED_DAMAGE_PCT);
    }
    if move_b == PetBrawlMove::Feint && move_a.is_direct_attack() && a_attack.landed {
        b_attack.damage = ceil_percent(b_attack.damage, FEINT_INTERRUPTED_DAMAGE_PCT);
    }

    let mut counter_to_a = 0;
    let mut counter_to_b = 0;
    let mut counter_a_crit = false;
    let mut counter_b_crit = false;
    if move_a == PetBrawlMove::Sidestep && move_b.is_direct_attack() && b_attack.landed {
        (counter_to_b, counter_a_crit) = sidestep_counter_damage(&state.a, &state.b, rng);
    }
    if move_b == PetBrawlMove::Sidestep && move_a.is_direct_attack() && a_attack.landed {
        (counter_to_a, counter_b_crit) = sidestep_counter_damage(&state.b, &state.a, rng);
    }
    let damage_to_a = b_attack.damage + counter_to_a;
    let damage_to_b = a_attack.damage + counter_to_b;
    let heal_a = if move_a == PetBrawlMove::Hunker && move_b != PetBrawlMove::Feint {
        hunker_heal(&state.a, rng)
    } else {
        0
    };
    let heal_b = if move_b == PetBrawlMove::Hunker && move_a != PetBrawlMove::Feint {
        hunker_heal(&state.b, rng)
    } else {
        0
    };

    let mut log = Vec::new();
    append_move_log(
        &mut log,
        &state.a,
        move_a,
        a_attack,
        counter_to_b,
        counter_a_crit,
    );
    append_move_log(
        &mut log,
        &state.b,
        move_b,
        b_attack,
        counter_to_a,
        counter_b_crit,
    );
    if move_a == PetBrawlMove::Feint && move_b == PetBrawlMove::Hunker {
        log.push(format!(
            "🎭 **{}** breaks **{}**'s guard — no recovery!",
            state.a.name, state.b.name
        ));
    }
    if move_b == PetBrawlMove::Feint && move_a == PetBrawlMove::Hunker {
        log.push(format!(
            "🎭 **{}** breaks **{}**'s guard — no recovery!",
            state.b.name, state.a.name
        ));
    }
    if heal_a != 0 {
        log.push(format!("💚 **{}** recovers {heal_a} HP.", state.a.name));
    }
    if heal_b != 0 {
        log.push(format!("💚 **{}** recovers {heal_b} HP.", state.b.name));
    }

    let mut a = state.a.clone();
    let mut b = state.b.clone();
    a.hp = a.max_hp.min(a.hp - damage_to_a + heal_a);
    b.hp = b.max_hp.min(b.hp - damage_to_b + heal_b);
    if a.hp <= 0 && a.shield_available {
        a.hp = 1;
        a.shield_available = false;
        log.push(format!(
            "🛡️ **{}**'s shell flashes — it survives on 1 HP!",
            a.name
        ));
    }
    if b.hp <= 0 && b.shield_available {
        b.hp = 1;
        b.shield_available = false;
        log.push(format!(
            "🛡️ **{}**'s shell flashes — it survives on 1 HP!",
            b.name
        ));
    }

    let round_no = state.round_no + 1;
    let winner = determine_winner(&a, &b, round_no, rng, &mut log);
    Ok((
        PetBrawlState {
            a,
            b,
            round_no,
            winner,
        },
        log,
    ))
}

fn append_move_log(
    log: &mut Vec<String>,
    attacker: &Duelist,
    move_: PetBrawlMove,
    attack: AttackResult,
    counter: i32,
    counter_crit: bool,
) {
    let label = move_name(&attacker.species_id, move_);
    match move_ {
        PetBrawlMove::Hunker => {
            log.push(format!("🛡️ **{}** hunkers down ({label}).", attacker.name))
        }
        PetBrawlMove::Sidestep if counter != 0 => {
            let flourish = if counter_crit {
                " with a CRITICAL counter"
            } else {
                " and counters"
            };
            log.push(format!(
                "🌀 **{}** sidesteps{flourish} for {counter}!",
                attacker.name
            ));
        }
        PetBrawlMove::Sidestep => log.push(format!(
            "🌀 **{}** sidesteps, but finds no opening.",
            attacker.name
        )),
        _ if !attack.landed => log.push(format!(
            "💨 **{}**'s {label} misses entirely!",
            attacker.name
        )),
        _ if attack.crit => log.push(format!(
            "💥 CRITICAL! **{}**'s {label} devastates for {}!",
            attacker.name, attack.damage
        )),
        _ => log.push(format!(
            "{} **{}**'s {label} hits for {}.",
            move_.emoji(),
            attacker.name,
            attack.damage
        )),
    }
}

fn determine_winner<R: BrawlRng>(
    a: &Duelist,
    b: &Duelist,
    round_no: u8,
    rng: &mut R,
    log: &mut Vec<String>,
) -> Option<BrawlWinner> {
    if a.hp <= 0 && b.hp <= 0 {
        let winner = if a.hp != b.hp {
            if a.hp > b.hp {
                BrawlWinner::A
            } else {
                BrawlWinner::B
            }
        } else if rng.randint(0, 1) == 0 {
            BrawlWinner::A
        } else {
            BrawlWinner::B
        };
        let standing = if winner == BrawlWinner::A { a } else { b };
        log.push(format!(
            "☄️ Double knockout! **{}** staggers up last!",
            standing.name
        ));
        Some(winner)
    } else if b.hp <= 0 {
        log.push(format!("🏆 **{}** is knocked out!", b.name));
        Some(BrawlWinner::A)
    } else if a.hp <= 0 {
        log.push(format!("🏆 **{}** is knocked out!", a.name));
        Some(BrawlWinner::B)
    } else if round_no >= MAX_ROUNDS {
        let hp_share_a = a.hp * b.max_hp;
        let hp_share_b = b.hp * a.max_hp;
        if hp_share_a == hp_share_b {
            log.push(format!(
                "🔔 The judges call a draw after {MAX_ROUNDS} rounds!"
            ));
            Some(BrawlWinner::Draw)
        } else {
            let winner = if hp_share_a > hp_share_b {
                BrawlWinner::A
            } else {
                BrawlWinner::B
            };
            let judge = if winner == BrawlWinner::A { a } else { b };
            log.push(format!(
                "🔔 The judges call it after {MAX_ROUNDS} rounds — **{}** takes it!",
                judge.name
            ));
            Some(winner)
        }
    } else {
        None
    }
}

/// Persisted challenge model. Round-by-round HP deliberately stays out of it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PetBrawl {
    pub brawl_id: i64,
    pub guild_id: i64,
    pub channel_id: i64,
    pub challenger_id: i64,
    pub recipient_id: i64,
    pub challenger_pet_id: i64,
    pub recipient_pet_id: Option<i64>,
    pub status: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub resolved_at: Option<i64>,
    pub winner_id: Option<i64>,
    pub winner_pet_id: Option<i64>,
    pub loser_pet_id: Option<i64>,
    pub rounds: Option<i64>,
    pub winner_hunger_delta: i64,
    pub loser_hunger_delta: i64,
    pub wager: i64,
    pub fee: i64,
    pub challenger_xp_delta: i64,
    pub recipient_xp_delta: i64,
    pub challenger_stat_gain: Option<String>,
    pub recipient_stat_gain: Option<String>,
    pub personality_event_key: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PetBrawlSnapshot<'a> {
    pub brawl_id: i64,
    pub guild_id: i64,
    pub channel_id: i64,
    pub challenger_id: i64,
    pub recipient_id: i64,
    pub challenger_pet_id: i64,
    pub recipient_pet_id: Option<i64>,
    pub status: &'a str,
    pub created_at: i64,
    pub expires_at: i64,
    pub resolved_at: Option<i64>,
    pub winner_id: Option<i64>,
    pub winner_pet_id: Option<i64>,
    pub loser_pet_id: Option<i64>,
    pub rounds: Option<i64>,
    pub winner_hunger_delta: i64,
    pub loser_hunger_delta: i64,
}

impl From<PetBrawlSnapshot<'_>> for PetBrawl {
    fn from(snapshot: PetBrawlSnapshot<'_>) -> Self {
        Self {
            brawl_id: snapshot.brawl_id,
            guild_id: snapshot.guild_id,
            channel_id: snapshot.channel_id,
            challenger_id: snapshot.challenger_id,
            recipient_id: snapshot.recipient_id,
            challenger_pet_id: snapshot.challenger_pet_id,
            recipient_pet_id: snapshot.recipient_pet_id,
            status: snapshot.status.to_owned(),
            created_at: snapshot.created_at,
            expires_at: snapshot.expires_at,
            resolved_at: snapshot.resolved_at,
            winner_id: snapshot.winner_id,
            winner_pet_id: snapshot.winner_pet_id,
            loser_pet_id: snapshot.loser_pet_id,
            rounds: snapshot.rounds,
            winner_hunger_delta: snapshot.winner_hunger_delta,
            loser_hunger_delta: snapshot.loser_hunger_delta,
            wager: 0,
            fee: 0,
            challenger_xp_delta: 0,
            recipient_xp_delta: 0,
            challenger_stat_gain: None,
            recipient_stat_gain: None,
            personality_event_key: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, VecDeque};

    use super::{
        BASE_CRIT_PCT, BRAWL_TRAIT_SPECIES, BrawlRng, BrawlTraits, BrawlWinner, Duelist,
        DuelistSnapshot, HUNKER_HEAL, MAX_ROUNDS, PetBrawl, PetBrawlMove, PetBrawlSnapshot,
        PetBrawlState, ResolveRoundError, SPITTER_CRIT_PCT, STAMPEDE_MISS_PCT, brawl_traits,
        build_duelist, has_move_flavor, initial_state, move_name, resolve_round, trait_blurb,
    };
    use crate::pet::{PetStage, SPECIES, SpeciesTier};

    #[derive(Debug)]
    struct ScriptedRng {
        values: VecDeque<i32>,
    }

    impl ScriptedRng {
        fn new(values: &[i32]) -> Self {
            Self {
                values: values.iter().copied().collect(),
            }
        }
    }

    impl BrawlRng for ScriptedRng {
        fn randint(&mut self, low: i32, high: i32) -> i32 {
            let value = self.values.pop_front().expect("scripted RNG exhausted");
            assert!(
                (low..=high).contains(&value),
                "scripted {value} outside [{low}, {high}]"
            );
            value
        }
    }

    /// Small deterministic generator used only for distribution/invariant tests.
    struct SeededRng {
        state: u64,
    }

    impl SeededRng {
        fn new(seed: u64) -> Self {
            Self {
                state: seed ^ 0xA076_1D64_78BD_642F,
            }
        }

        fn next(&mut self) -> u64 {
            self.state ^= self.state >> 12;
            self.state ^= self.state << 25;
            self.state ^= self.state >> 27;
            self.state = self.state.wrapping_mul(0x2545_F491_4F6C_DD1D);
            self.state
        }

        fn choose_move(&mut self) -> PetBrawlMove {
            PetBrawlMove::ALL[(self.next() % PetBrawlMove::ALL.len() as u64) as usize]
        }
    }

    impl BrawlRng for SeededRng {
        fn randint(&mut self, low: i32, high: i32) -> i32 {
            let width = u64::try_from(high - low + 1).expect("valid RNG bounds");
            low + i32::try_from(self.next() % width).expect("bounded value fits i32")
        }
    }

    fn mk(species_id: &str) -> Duelist {
        build_duelist(DuelistSnapshot {
            pet_id: 1,
            owner_id: 100,
            name: species_id,
            species_id,
            stage: PetStage::Adult,
            hunger: 100,
            happy: false,
            aegis_used: 0,
            training_str: 0,
            training_int: 0,
            training_dex: 0,
        })
    }

    fn named(species_id: &str, name: &str) -> Duelist {
        let mut duelist = mk(species_id);
        duelist.name = name.to_owned();
        duelist
    }

    fn scripted_round(
        state: &PetBrawlState,
        move_a: PetBrawlMove,
        move_b: PetBrawlMove,
        values: &[i32],
    ) -> (PetBrawlState, Vec<String>) {
        resolve_round(state, move_a, move_b, &mut ScriptedRng::new(values))
            .expect("live brawl resolves")
    }

    fn log_contains(log: &[String], needle: &str) -> bool {
        log.iter().any(|line| line.contains(needle))
    }

    #[test]
    fn test_pet_brawl_economy_and_training_fields_default_to_free_and_unawarded() {
        let brawl = PetBrawl::from(PetBrawlSnapshot {
            brawl_id: 1,
            guild_id: 100,
            channel_id: 5,
            challenger_id: 10,
            recipient_id: 20,
            challenger_pet_id: 1_000,
            recipient_pet_id: None,
            status: "pending",
            created_at: 1_000,
            expires_at: 1_300,
            resolved_at: None,
            winner_id: None,
            winner_pet_id: None,
            loser_pet_id: None,
            rounds: None,
            winner_hunger_delta: 0,
            loser_hunger_delta: 0,
        });

        assert_eq!(brawl.wager, 0);
        assert_eq!(brawl.fee, 0);
        assert_eq!(brawl.challenger_xp_delta, 0);
        assert_eq!(brawl.recipient_xp_delta, 0);
        assert_eq!(brawl.challenger_stat_gain, None);
        assert_eq!(brawl.recipient_stat_gain, None);
        assert_eq!(brawl.personality_event_key, None);
    }

    #[test]
    fn test_hp_and_power_derivation() {
        let cases = [
            ("common_cama", true, 100, false, 100, 100, 0),
            ("common_cama", false, 100, false, 80, 80, 0),
            ("common_cama", true, 0, false, 100, 80, 0),
            ("common_cama", true, 55, true, 100, 91, 1),
            ("jopacama", true, 100, false, 100, 100, 1),
            ("aegis_cama", false, 40, false, 80, 68, 1),
            ("rama", true, 100, true, 100, 100, 3),
        ];
        for (species_id, adult, hunger, happy, max_hp, hp, power) in cases {
            let duelist = build_duelist(DuelistSnapshot {
                pet_id: 1,
                owner_id: 100,
                name: species_id,
                species_id,
                stage: if adult {
                    PetStage::Adult
                } else {
                    PetStage::Baby
                },
                hunger,
                happy,
                aegis_used: 0,
                training_str: 0,
                training_int: 0,
                training_dex: 0,
            });
            assert_eq!(
                (duelist.max_hp, duelist.hp, duelist.power),
                (max_hp, hp, power)
            );
        }
    }

    #[test]
    fn test_shield_gated_on_unspent_aegis() {
        assert!(mk("aegis_cama").shield_available);
        let mut spent = mk("aegis_cama");
        spent.shield_available = build_duelist(DuelistSnapshot {
            aegis_used: 1,
            ..DuelistSnapshot {
                pet_id: 1,
                owner_id: 100,
                name: "aegis_cama",
                species_id: "aegis_cama",
                stage: PetStage::Adult,
                hunger: 100,
                happy: false,
                aegis_used: 0,
                training_str: 0,
                training_int: 0,
                training_dex: 0,
            }
        })
        .shield_available;
        assert!(!spent.shield_available);
        assert!(!mk("common_cama").shield_available);
    }

    #[test]
    fn test_unknown_species_uses_fallback() {
        let duelist = mk("retired_species");
        assert_eq!(duelist.tier, SpeciesTier::Common);
        assert_eq!(duelist.power, 0);
    }

    #[test]
    fn test_training_stats_are_snapshotted_without_changing_base_power() {
        let duelist = build_duelist(DuelistSnapshot {
            training_str: 2,
            training_int: 1,
            training_dex: 2,
            ..DuelistSnapshot {
                pet_id: 1,
                owner_id: 100,
                name: "common_cama",
                species_id: "common_cama",
                stage: PetStage::Adult,
                hunger: 100,
                happy: false,
                aegis_used: 0,
                training_str: 0,
                training_int: 0,
                training_dex: 0,
            }
        });
        assert_eq!(
            (
                duelist.training_str,
                duelist.training_int,
                duelist.training_dex
            ),
            (2, 1, 2)
        );
        assert_eq!(duelist.power, 0);
    }

    #[test]
    fn test_species_flavored_names() {
        assert_eq!(move_name("rama", PetBrawlMove::Spit), "Royal Spit");
        assert_eq!(
            move_name("crystal_cama", PetBrawlMove::Stampede),
            "Blizzard Stampede"
        );
    }

    #[test]
    fn test_unknown_species_falls_back_to_defaults() {
        assert_eq!(move_name("retired_species", PetBrawlMove::Spit), "Spit");
        assert_eq!(
            move_name("retired_species", PetBrawlMove::Hunker),
            "Hunker Down"
        );
    }

    #[test]
    fn test_every_species_flavors_the_two_new_moves() {
        for species in SPECIES {
            assert!(has_move_flavor(species.species_id, PetBrawlMove::Feint));
            assert!(has_move_flavor(species.species_id, PetBrawlMove::Sidestep));
        }
    }

    #[test]
    fn test_new_species_have_themed_move_names() {
        let expected = [
            ("embergear_cama", PetBrawlMove::Spit, "Spark Spit"),
            ("embergear_cama", PetBrawlMove::Stampede, "Overdrive"),
            ("embergear_cama", PetBrawlMove::Hunker, "Vent Heat"),
            ("riverglow_cama", PetBrawlMove::Spit, "Charged Spit"),
            ("riverglow_cama", PetBrawlMove::Stampede, "River Rush"),
            ("riverglow_cama", PetBrawlMove::Hunker, "Bottle Up"),
            ("prismwool_cama", PetBrawlMove::Spit, "Prismatic Spit"),
            ("prismwool_cama", PetBrawlMove::Stampede, "Linked Charge"),
            ("prismwool_cama", PetBrawlMove::Hunker, "Wool Ward"),
            ("moondrift_cama", PetBrawlMove::Spit, "Moonlit Spit"),
            ("moondrift_cama", PetBrawlMove::Stampede, "Tidal Rush"),
            ("moondrift_cama", PetBrawlMove::Hunker, "Eclipse"),
            ("sunspun_cama", PetBrawlMove::Spit, "Solar Flare"),
            ("sunspun_cama", PetBrawlMove::Stampede, "Dawn Charge"),
            ("sunspun_cama", PetBrawlMove::Hunker, "Corona Guard"),
        ];
        for (species_id, move_, expected_name) in expected {
            assert_eq!(move_name(species_id, move_), expected_name);
        }
    }

    #[test]
    fn test_every_move_has_emoji() {
        for move_ in PetBrawlMove::ALL {
            assert!(!move_.emoji().is_empty());
        }
    }

    #[test]
    fn test_every_move_has_a_blurb() {
        for move_ in PetBrawlMove::ALL {
            assert!(!move_.blurb().is_empty());
        }
    }

    #[test]
    fn test_every_species_has_an_explicit_entry() {
        // Quirkless entries are explicit in the authoritative 15-species table.
        let explicit: BTreeSet<_> = BRAWL_TRAIT_SPECIES.into_iter().collect();
        assert_eq!(
            explicit,
            SPECIES.iter().map(|species| species.species_id).collect()
        );
    }

    #[test]
    fn test_unknown_species_falls_back_to_no_traits() {
        assert_eq!(brawl_traits("retired_species"), BrawlTraits::default());
    }

    #[test]
    fn test_new_legendary_species_have_distinct_traits() {
        assert_eq!(
            brawl_traits("moondrift_cama"),
            BrawlTraits {
                dodge_bonus_pp: 15,
                ..BrawlTraits::default()
            }
        );
        assert_eq!(
            brawl_traits("sunspun_cama"),
            BrawlTraits {
                heal_bonus: 5,
                ..BrawlTraits::default()
            }
        );
    }

    #[test]
    fn test_trait_blurbs_cover_exactly_the_quirked_species() {
        for species in SPECIES {
            assert_eq!(
                trait_blurb(species.species_id).is_some(),
                brawl_traits(species.species_id) != BrawlTraits::default(),
                "{}",
                species.species_id
            );
        }
    }

    #[test]
    fn test_moondrift_evades_more_stampedes() {
        let state = initial_state(named("common_cama", "A"), named("moondrift_cama", "B"));
        let (resolved, _) = scripted_round(
            &state,
            PetBrawlMove::Stampede,
            PetBrawlMove::Spit,
            &[70, 16, 100, 8, 100],
        );
        assert_eq!(resolved.b.hp, 100);
    }

    #[test]
    fn test_sunspun_recovers_more_while_hunkered() {
        let mut b = named("sunspun_cama", "B");
        b.hp = 70;
        let state = initial_state(named("common_cama", "A"), b);
        let (resolved, _) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Hunker,
            &[8, 100, 5],
        );
        assert_eq!(resolved.a.hp, 100);
        assert_eq!(resolved.b.hp, 77);
    }

    #[test]
    fn test_strength_adds_all_attack_damage_and_extra_stampede_damage() {
        let mut a = named("common_cama", "A");
        a.training_str = 1;
        let spit_state = initial_state(a.clone(), named("common_cama", "B"));
        let (spit, _) = scripted_round(
            &spit_state,
            PetBrawlMove::Spit,
            PetBrawlMove::Spit,
            &[8, 100, 8, 100],
        );
        assert_eq!(spit.b.hp, 100 - 9);

        let stampede_state = initial_state(a, named("common_cama", "B"));
        let (stampede, _) = scripted_round(
            &stampede_state,
            PetBrawlMove::Stampede,
            PetBrawlMove::Spit,
            &[100, 16, 100, 8, 100],
        );
        assert_eq!(stampede.b.hp, 100 - 18);
    }

    #[test]
    fn test_dexterity_improves_general_crit_spit_crit_and_stampede_evasion() {
        let mut a = named("common_cama", "A");
        a.training_dex = 1;
        let spit_state = initial_state(a, named("common_cama", "B"));
        let (spit, _) = scripted_round(
            &spit_state,
            PetBrawlMove::Spit,
            PetBrawlMove::Spit,
            &[8, 14, 8, 100],
        );
        assert_eq!(spit.b.hp, 100 - 16);

        let mut b = named("common_cama", "B");
        b.training_dex = 1;
        let dodge_state = initial_state(named("common_cama", "A"), b);
        let (dodge, _) = scripted_round(
            &dodge_state,
            PetBrawlMove::Stampede,
            PetBrawlMove::Spit,
            &[STAMPEDE_MISS_PCT + 1, 8, 100],
        );
        assert_eq!(dodge.b.hp, 100);
    }

    #[test]
    fn test_intelligence_reduces_damage_and_improves_hunker() {
        let mut b = named("common_cama", "B");
        b.training_int = 1;
        b.hp = 70;
        let state = initial_state(named("common_cama", "A"), b);
        let (resolved, _) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Hunker,
            &[8, 100, 5],
        );
        assert_eq!(resolved.b.hp, 70 - 2 + 6);
        assert_eq!(resolved.a.hp, 100);
    }

    #[test]
    fn test_spit_deals_scripted_damage_both_ways() {
        let state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
        let (new_state, log) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Spit,
            &[10, 100, 12, 100],
        );
        assert_eq!(new_state.a.hp, 100 - 12);
        assert_eq!(new_state.b.hp, 100 - 10);
        assert_eq!(new_state.winner, None);
        assert_eq!(new_state.round_no, 1);
        assert!(log_contains(&log, "hits for 10"));
    }

    #[test]
    fn test_crit_doubles_damage() {
        let state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
        let (new_state, log) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Spit,
            &[10, 1, 12, 100],
        );
        assert_eq!(new_state.b.hp, 100 - 20);
        assert!(log_contains(&log, "CRITICAL"));
    }

    #[test]
    fn test_stampede_can_miss() {
        let state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
        let (new_state, log) = scripted_round(
            &state,
            PetBrawlMove::Stampede,
            PetBrawlMove::Spit,
            &[STAMPEDE_MISS_PCT, 8, 100],
        );
        assert_eq!(new_state.b.hp, 100);
        assert!(log_contains(&log, "misses"));
    }

    #[test]
    fn test_hunker_softens_spit_and_heals_without_countering() {
        let state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
        let (new_state, _) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Hunker,
            &[11, 100, 5],
        );
        assert_eq!(new_state.b.hp, 100);
        assert_eq!(new_state.a.hp, 100);
    }

    #[test]
    fn test_hunker_absorbs_most_spit_damage_without_countering() {
        let state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
        let (new_state, _) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Hunker,
            &[16, 100, 5],
        );
        assert_eq!(new_state.b.hp, 100);
        assert_eq!(new_state.a.hp, 100);
    }

    #[test]
    fn test_feint_breaks_hunker_and_prevents_recovery() {
        let mut b = named("common_cama", "B");
        b.hp = 70;
        let state = initial_state(named("common_cama", "A"), b);
        let (new_state, _) = scripted_round(
            &state,
            PetBrawlMove::Feint,
            PetBrawlMove::Hunker,
            &[10, 100, 2],
        );
        assert_eq!(new_state.b.hp, 60);
        assert_eq!(new_state.a.hp, 100);
    }

    #[test]
    fn test_direct_attack_interrupts_feint_damage() {
        let state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
        let (new_state, _) = scripted_round(
            &state,
            PetBrawlMove::Feint,
            PetBrawlMove::Spit,
            &[10, 100, 8, 100],
        );
        assert_eq!(new_state.a.hp, 92);
        assert_eq!(new_state.b.hp, 95);
    }

    #[test]
    fn test_stampede_is_more_accurate_against_feint_but_can_still_miss() {
        let state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
        let (new_state, log) = scripted_round(
            &state,
            PetBrawlMove::Stampede,
            PetBrawlMove::Feint,
            &[18, 12, 100],
        );
        assert!(new_state.a.hp < 100);
        assert_eq!(new_state.b.hp, 100);
        assert!(
            log.iter()
                .any(|line| line.contains('A') && line.contains("misses"))
        );
    }

    #[test]
    fn test_stampede_hits_feint_on_a_roll_that_misses_a_normal_target() {
        let feint_state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
        let (feint_result, feint_log) = scripted_round(
            &feint_state,
            PetBrawlMove::Stampede,
            PetBrawlMove::Feint,
            &[41, 16, 100, 8, 100],
        );
        let normal_state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
        let (normal_result, normal_log) = scripted_round(
            &normal_state,
            PetBrawlMove::Stampede,
            PetBrawlMove::Spit,
            &[41, 8, 100],
        );
        assert_eq!(feint_result.b.hp, 84);
        assert!(
            !feint_log
                .iter()
                .any(|line| line.contains('A') && line.contains("misses"))
        );
        assert_eq!(normal_result.b.hp, 100);
        assert!(
            normal_log
                .iter()
                .any(|line| line.contains('A') && line.contains("misses"))
        );
    }

    #[test]
    fn test_sidestep_reduces_a_direct_hit_and_returns_light_damage() {
        let state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
        let (new_state, log) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Sidestep,
            &[12, 100, 8, 100],
        );
        assert_eq!(new_state.a.hp, 92);
        assert_eq!(new_state.b.hp, 97);
        assert!(log_contains(&log, "counters for 8"));
    }

    #[test]
    fn test_feint_catches_sidestep_without_drawing_a_counter() {
        let state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
        let (new_state, _) = scripted_round(
            &state,
            PetBrawlMove::Feint,
            PetBrawlMove::Sidestep,
            &[10, 100, 8, 100],
        );
        assert_eq!(new_state.a.hp, 100);
        assert_eq!(new_state.b.hp, 90);
    }

    #[test]
    fn test_sidestep_finds_no_opening_against_hunker() {
        let state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
        let (new_state, log) =
            scripted_round(&state, PetBrawlMove::Hunker, PetBrawlMove::Sidestep, &[5]);
        assert_eq!(new_state.a.hp, 100);
        assert_eq!(new_state.b.hp, 100);
        assert!(log_contains(&log, "no opening"));
    }

    #[test]
    fn test_stampede_cannot_miss_a_hunkered_opponent() {
        let mut b = named("common_cama", "B");
        b.hp = 50;
        let state = initial_state(named("common_cama", "A"), b);
        let (new_state, log) = scripted_round(
            &state,
            PetBrawlMove::Stampede,
            PetBrawlMove::Hunker,
            &[16, 100, 5],
        );
        assert!(new_state.b.hp < 50);
        assert_eq!(new_state.a.hp, 100);
        assert!(!log_contains(&log, "misses"));
    }

    #[test]
    fn test_mystic_hunker_is_also_pure_defense() {
        let state = initial_state(named("common_cama", "A"), named("invoker_cama", "B"));
        let (new_state, _) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Hunker,
            &[11, 100, 5],
        );
        assert_eq!(new_state.a.hp, 100);
    }

    #[test]
    fn test_dune_reduces_damage_with_floor_one() {
        let state = initial_state(named("common_cama", "A"), named("dromedary_cross", "B"));
        let (new_state, _) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Spit,
            &[8, 100, 8, 100],
        );
        assert_eq!(new_state.b.hp, 100 - 7);

        let mut b = named("dromedary_cross", "B");
        b.hp = 50;
        let state = initial_state(named("common_cama", "A"), b);
        let (new_state, _) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Hunker,
            &[8, 100, 5],
        );
        assert_eq!(new_state.b.hp, 50 - 2 + 5);
    }

    #[test]
    fn test_ravenous_deals_and_takes_extra() {
        let state = initial_state(named("pudge_cama", "A"), named("common_cama", "B"));
        let (new_state, _) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Spit,
            &[10, 100, 10, 100],
        );
        assert_eq!(new_state.b.hp, 100 - 12);
        assert_eq!(new_state.a.hp, 100 - 11);
    }

    #[test]
    fn test_ravenous_hunker_heals_extra() {
        let mut b = named("pudge_cama", "B");
        b.hp = 80;
        let state = initial_state(named("common_cama", "A"), b);
        let (new_state, _) =
            scripted_round(&state, PetBrawlMove::Hunker, PetBrawlMove::Hunker, &[5, 5]);
        assert_eq!(new_state.b.hp, 80 + 8);
    }

    #[test]
    fn test_frostwool_makes_stampedes_miss_more() {
        let state = initial_state(named("common_cama", "A"), named("crystal_cama", "B"));
        let (new_state, _) = scripted_round(
            &state,
            PetBrawlMove::Stampede,
            PetBrawlMove::Spit,
            &[STAMPEDE_MISS_PCT + 10, 8, 100],
        );
        assert_eq!(new_state.b.hp, 100);
    }

    #[test]
    fn test_shellback_shield_saves_once() {
        let mut b = named("aegis_cama", "B");
        b.hp = 5;
        let state = initial_state(named("common_cama", "A"), b);
        let (new_state, log) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Spit,
            &[10, 100, 8, 100],
        );
        assert_eq!(new_state.b.hp, 1);
        assert!(!new_state.b.shield_available);
        assert!(log_contains(&log, "survives on 1 HP"));
        let (second, _) = scripted_round(
            &new_state,
            PetBrawlMove::Spit,
            PetBrawlMove::Spit,
            &[10, 100, 8, 100],
        );
        assert_eq!(second.winner, Some(BrawlWinner::A));
    }

    #[test]
    fn test_shellback_with_spent_aegis_has_no_shield() {
        let mut b = named("aegis_cama", "B");
        b.hp = 5;
        b.shield_available = false;
        let state = initial_state(named("common_cama", "A"), b);
        let (new_state, _) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Spit,
            &[10, 100, 8, 100],
        );
        assert_eq!(new_state.winner, Some(BrawlWinner::A));
    }

    #[test]
    fn test_simultaneous_ko_less_negative_hp_wins() {
        let mut a = named("common_cama", "A");
        a.hp = 5;
        let mut b = named("common_cama", "B");
        b.hp = 10;
        let state = initial_state(a, b);
        let (new_state, log) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Spit,
            &[16, 100, 16, 100],
        );
        assert_eq!(new_state.winner, Some(BrawlWinner::B));
        assert!(log_contains(&log, "Double knockout"));
    }

    #[test]
    fn test_simultaneous_ko_exact_tie_coin_flips() {
        let mut a = named("common_cama", "A");
        a.hp = 5;
        let mut b = named("common_cama", "B");
        b.hp = 5;
        let state = initial_state(a, b);
        let (new_state, _) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Spit,
            &[16, 100, 16, 100, 1],
        );
        assert_eq!(new_state.winner, Some(BrawlWinner::B));
    }

    #[test]
    fn test_round_cap_forces_winner_by_hp_pct() {
        let mut b = named("common_cama", "B");
        b.hp = 50;
        let state = PetBrawlState {
            a: named("common_cama", "A"),
            b,
            round_no: MAX_ROUNDS - 1,
            winner: None,
        };
        let (new_state, log) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Spit,
            &[10, 100, 10, 100],
        );
        assert_eq!(new_state.winner, Some(BrawlWinner::A));
        assert!(log_contains(&log, "judges"));
    }

    #[test]
    fn test_round_cap_equal_hp_percentage_is_a_draw() {
        let mut a = named("common_cama", "A");
        a.hp = 60;
        let mut b = build_duelist(DuelistSnapshot {
            stage: PetStage::Baby,
            name: "B",
            ..DuelistSnapshot {
                pet_id: 1,
                owner_id: 100,
                name: "common_cama",
                species_id: "common_cama",
                stage: PetStage::Adult,
                hunger: 100,
                happy: false,
                aegis_used: 0,
                training_str: 0,
                training_int: 0,
                training_dex: 0,
            }
        });
        b.hp = 48;
        let state = PetBrawlState {
            a,
            b,
            round_no: MAX_ROUNDS - 1,
            winner: None,
        };
        let (new_state, log) = scripted_round(
            &state,
            PetBrawlMove::Spit,
            PetBrawlMove::Spit,
            &[8, 100, 10, 100, 0],
        );
        assert_eq!((new_state.a.hp, new_state.a.max_hp), (50, 100));
        assert_eq!((new_state.b.hp, new_state.b.max_hp), (40, 80));
        assert_eq!(new_state.winner, Some(BrawlWinner::Draw));
        assert!(log.iter().any(|line| line.to_lowercase().contains("draw")));
    }

    #[test]
    fn test_resolve_on_finished_state_raises() {
        let state = PetBrawlState {
            a: named("common_cama", "A"),
            b: named("common_cama", "B"),
            round_no: 3,
            winner: Some(BrawlWinner::A),
        };
        assert_eq!(
            resolve_round(
                &state,
                PetBrawlMove::Spit,
                PetBrawlMove::Spit,
                &mut ScriptedRng::new(&[])
            ),
            Err(ResolveRoundError::AlreadyResolved)
        );
    }

    #[test]
    fn test_hunker_standoff_is_called_after_eight_rounds() {
        let mut rng = SeededRng::new(0);
        let mut state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
        let final_log = loop {
            let (next, log) =
                resolve_round(&state, PetBrawlMove::Hunker, PetBrawlMove::Hunker, &mut rng)
                    .expect("live brawl resolves");
            state = next;
            if state.winner.is_some() {
                break log;
            }
        };
        assert_eq!(state.round_no, 8);
        assert_eq!(state.winner, Some(BrawlWinner::Draw));
        assert!(log_contains(&final_log, "judges"));
    }

    #[test]
    fn test_any_seed_terminates_by_max_rounds() {
        for seed in 0..60 {
            let mut rng = SeededRng::new(seed);
            let mut a = named("rama", "A");
            a.power += 1;
            let mut state = initial_state(a, named("aegis_cama", "B"));
            let mut rounds = 0;
            while state.winner.is_none() {
                let move_a = rng.choose_move();
                let move_b = rng.choose_move();
                (state, _) =
                    resolve_round(&state, move_a, move_b, &mut rng).expect("live brawl resolves");
                rounds += 1;
                assert!(rounds <= MAX_ROUNDS);
            }
            assert!(matches!(
                state.winner,
                Some(BrawlWinner::A | BrawlWinner::B | BrawlWinner::Draw)
            ));
        }
    }

    #[test]
    fn test_no_single_move_spam_dominates_uniform_five_move_choices() {
        let trials: u32 = 1_000;
        for fixed_move in PetBrawlMove::ALL {
            let mut score = 0.0;
            for seed in 0..trials {
                let mut chooser = SeededRng::new(u64::from(seed ^ 0x5A17));
                let mut combat = SeededRng::new(u64::from(seed));
                let mut state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
                while state.winner.is_none() {
                    let opponent_move = chooser.choose_move();
                    (state, _) = resolve_round(&state, fixed_move, opponent_move, &mut combat)
                        .expect("live brawl resolves");
                }
                score += match state.winner {
                    Some(BrawlWinner::A) => 1.0,
                    Some(BrawlWinner::Draw) => 0.5,
                    Some(BrawlWinner::B) => 0.0,
                    None => unreachable!(),
                };
            }
            assert!(score / f64::from(trials) < 0.75, "{fixed_move:?}");
        }
    }

    #[test]
    fn test_hunker_neutralizes_repeated_spit_without_dealing_damage() {
        let mut a_wins = 0;
        let mut draws = 0;
        for seed in 0..300 {
            let mut rng = SeededRng::new(seed);
            let mut state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
            while state.winner.is_none() {
                (state, _) =
                    resolve_round(&state, PetBrawlMove::Hunker, PetBrawlMove::Spit, &mut rng)
                        .expect("live brawl resolves");
            }
            if state.winner == Some(BrawlWinner::A) {
                a_wins += 1;
            }
            if state.winner == Some(BrawlWinner::Draw) {
                draws += 1;
            }
            assert_eq!(state.b.hp, state.b.max_hp);
        }
        assert_eq!(a_wins, 0);
        assert!(draws >= 180);
    }

    #[test]
    fn test_repeated_hunker_never_defeats_an_attacking_opponent() {
        for seed in 0..100 {
            let mut rng = SeededRng::new(seed);
            let mut state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
            while state.winner.is_none() {
                (state, _) =
                    resolve_round(&state, PetBrawlMove::Hunker, PetBrawlMove::Spit, &mut rng)
                        .expect("live brawl resolves");
            }
            assert!(matches!(
                state.winner,
                Some(BrawlWinner::B | BrawlWinner::Draw)
            ));
        }
    }

    #[test]
    fn test_stampede_miss_rate_near_tuning() {
        let mut rng = SeededRng::new(7);
        let mut misses = 0;
        let trials = 3_000;
        for _ in 0..trials {
            let state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
            let (_, log) =
                resolve_round(&state, PetBrawlMove::Stampede, PetBrawlMove::Spit, &mut rng)
                    .expect("live brawl resolves");
            misses += i32::from(log_contains(&log, "misses"));
        }
        let observed = f64::from(misses) / f64::from(trials);
        assert!((observed - f64::from(STAMPEDE_MISS_PCT) / 100.0).abs() < 0.04);
    }

    #[test]
    fn test_rama_crits_more_than_base() {
        fn crit_rate(species_id: &str, rng: &mut SeededRng) -> f64 {
            let mut crits = 0;
            let trials = 3_000;
            for _ in 0..trials {
                let state = initial_state(named(species_id, "A"), named("common_cama", "B"));
                let (_, log) = resolve_round(&state, PetBrawlMove::Spit, PetBrawlMove::Hunker, rng)
                    .expect("live brawl resolves");
                crits += i32::from(log_contains(&log, "CRITICAL"));
            }
            f64::from(crits) / f64::from(trials)
        }

        let mut rng = SeededRng::new(11);
        assert!((crit_rate("rama", &mut rng) - f64::from(SPITTER_CRIT_PCT) / 100.0).abs() < 0.04);
        assert!(
            (crit_rate("common_cama", &mut rng) - f64::from(BASE_CRIT_PCT) / 100.0).abs() < 0.04
        );
    }

    #[test]
    fn test_spit_always_lands_without_drawing_a_counter() {
        let mut rng = SeededRng::new(3);
        for _ in 0..200 {
            let state = initial_state(named("common_cama", "A"), named("common_cama", "B"));
            let (new_state, _) =
                resolve_round(&state, PetBrawlMove::Spit, PetBrawlMove::Hunker, &mut rng)
                    .expect("live brawl resolves");
            assert!(new_state.b.hp <= 100);
            assert_eq!(new_state.a.hp, 100);
        }
    }

    #[test]
    fn test_hunker_heal_within_range() {
        let mut rng = SeededRng::new(5);
        for _ in 0..200 {
            let mut a = named("common_cama", "A");
            a.hp = 50;
            let state = initial_state(a, named("common_cama", "B"));
            let (new_state, _) =
                resolve_round(&state, PetBrawlMove::Hunker, PetBrawlMove::Spit, &mut rng)
                    .expect("live brawl resolves");
            assert!(new_state.a.hp <= 50 + HUNKER_HEAL.1);
        }
    }
}
