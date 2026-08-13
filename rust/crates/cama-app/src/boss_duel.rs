//! Stateful, transport-independent policy for regular boss HP duels.
//!
//! Python remains the live Discord and SQLite implementation.  This module is
//! additive: it models the same combat, pause/resume, phase-event, echo,
//! durability, and settlement boundaries behind typed values.  A future
//! runtime adapter can implement [`BossDuelRepositoryPort`] against the shared
//! schema without giving the parity process a second live writer.

use std::collections::{BTreeMap, BTreeSet};

use cama_domain::dig_economy::{
    WagerMultiplier, WinChance, effective_wager_multiplier, scale_positive_dig_jc,
    settled_wager_multiplier,
};

use crate::dig_bosses::{
    CombatStats, DuelOddsInput, approximate_duel_win_probability, scale_boss_stats,
};

pub const PET_TRAINING_CAP: i32 = 20;
pub const PLAYER_HIT_FLOOR: f64 = 0.05;
pub const WAGERED_PLAYER_HIT_FLOOR: f64 = 0.10;
pub const PLAYER_HIT_CEILING: f64 = 0.90;
pub const FREE_FIGHT_ACCURACY_MULTIPLIER: f64 = 0.70;
pub const ECHO_HP_MULTIPLIER: f64 = 0.75;
pub const ECHO_PROFIT_MULTIPLIER: f64 = 0.70;
pub const ECHO_WINDOW_SECONDS: i64 = 24 * 60 * 60;
pub const BOSS_LOSS_KNOCKBACK_MIN: i32 = 11;
pub const BOSS_LOSS_KNOCKBACK_MAX: i32 = 20;
pub const BOSS_LOSS_REPAIR_BILL: i64 = 8;
pub const BOSS_LOSS_EXTRA_COOLDOWN_SECONDS: i64 = 3_600;
pub const BOSS_LOSS_EXTRA_GEAR_TICKS: i32 = 1;
pub const BOSS_STAT_POINT_BONUS: i32 = 1;

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

pub trait BossDuelRng {
    fn next_unit(&mut self) -> f64;
    fn choose_index(&mut self, upper_bound: usize) -> usize;
}

#[derive(Clone, Debug)]
pub struct SequenceRng {
    rolls: Vec<f64>,
    choices: Vec<usize>,
    roll_cursor: usize,
    choice_cursor: usize,
}

impl SequenceRng {
    #[must_use]
    pub fn new(rolls: Vec<f64>, choices: Vec<usize>) -> Self {
        Self {
            rolls,
            choices,
            roll_cursor: 0,
            choice_cursor: 0,
        }
    }
}

impl BossDuelRng for SequenceRng {
    fn next_unit(&mut self) -> f64 {
        let value = self.rolls.get(self.roll_cursor).copied().unwrap_or(0.99);
        self.roll_cursor += 1;
        value
    }

    fn choose_index(&mut self, upper_bound: usize) -> usize {
        if upper_bound == 0 {
            return 0;
        }
        let value = self.choices.get(self.choice_cursor).copied().unwrap_or(0);
        self.choice_cursor += 1;
        value % upper_bound
    }
}

#[must_use]
pub fn fractional_damage_bonus<R: BossDuelRng>(
    damage: i32,
    bonus_percent: i32,
    rng: &mut R,
) -> i32 {
    let scaled = damage.max(0) * bonus_percent.max(0);
    let guaranteed = scaled / 100;
    let remainder = scaled % 100;
    guaranteed + i32::from(remainder > 0 && rng.next_unit() < f64::from(remainder) / 100.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DuelStats {
    pub player_hp: i32,
    pub boss_hp: i32,
    pub player_hit: f64,
    pub player_damage: i32,
    pub boss_hit: f64,
    pub boss_damage: i32,
    pub critical_chance: f64,
    pub critical_bonus: i32,
}

impl DuelStats {
    #[must_use]
    pub const fn cautious() -> Self {
        Self {
            player_hp: 5,
            boss_hp: 4,
            player_hit: 0.60,
            player_damage: 1,
            boss_hit: 0.30,
            boss_damage: 1,
            critical_chance: 0.0,
            critical_bonus: 0,
        }
    }

    #[must_use]
    pub const fn reckless() -> Self {
        Self {
            player_hp: 2,
            boss_hp: 6,
            player_hit: 0.18,
            player_damage: 3,
            boss_hit: 0.60,
            boss_damage: 1,
            critical_chance: 0.30,
            critical_bonus: 1,
        }
    }

    #[must_use]
    pub fn scaled(self, boss_id: &str, depth: i32, prestige: i32, echo: bool) -> Self {
        let scaled = scale_boss_stats(
            CombatStats {
                player_hp: self.player_hp,
                boss_hp: self.boss_hp,
                player_hit: self.player_hit,
                player_damage: self.player_damage,
                boss_hit: self.boss_hit,
                boss_damage: self.boss_damage,
            },
            boss_id,
            depth,
            prestige,
            echo,
        );
        Self {
            player_hp: scaled.player_hp,
            boss_hp: scaled.boss_hp,
            player_hit: scaled.player_hit,
            player_damage: scaled.player_damage,
            boss_hit: scaled.boss_hit,
            boss_damage: scaled.boss_damage,
            ..self
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoundRecord {
    pub round: u8,
    pub player_hp: i32,
    pub boss_hp: i32,
    pub player_hit: bool,
    pub boss_hit: Option<bool>,
    pub pet_assist: Option<PetAssist>,
    pub pet_assist_damage: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoundOutcome {
    pub record: RoundRecord,
    pub terminal: Option<bool>,
}

#[must_use]
pub fn run_round<R: BossDuelRng>(
    round: u8,
    stats: &mut DuelStats,
    assist: Option<&PetAssist>,
    rng: &mut R,
) -> RoundOutcome {
    let hit = rng.next_unit() < stats.player_hit;
    let mut assist_damage = 0;
    if hit {
        let mut damage = stats.player_damage;
        if stats.critical_chance > 0.0 && rng.next_unit() < stats.critical_chance {
            damage += stats.critical_bonus;
        }
        if let Some(pet) = assist {
            assist_damage = fractional_damage_bonus(damage, pet.bonus_percent, rng);
            damage += assist_damage;
        }
        stats.boss_hp = (stats.boss_hp - damage).max(0);
    }
    if stats.boss_hp == 0 {
        return RoundOutcome {
            record: RoundRecord {
                round,
                player_hp: stats.player_hp,
                boss_hp: 0,
                player_hit: hit,
                boss_hit: None,
                pet_assist: assist.cloned(),
                pet_assist_damage: assist_damage,
            },
            terminal: Some(true),
        };
    }
    let boss_hit = rng.next_unit() < stats.boss_hit;
    if boss_hit {
        stats.player_hp = (stats.player_hp - stats.boss_damage).max(0);
    }
    RoundOutcome {
        record: RoundRecord {
            round,
            player_hp: stats.player_hp,
            boss_hp: stats.boss_hp,
            player_hit: hit,
            boss_hit: Some(boss_hit),
            pet_assist: assist.cloned(),
            pet_assist_damage: assist_damage,
        },
        terminal: (stats.player_hp == 0).then_some(false),
    }
}

#[must_use]
pub fn exact_duel_win_probability(stats: DuelStats, player_damage_bonus_percent: i32) -> f64 {
    if player_damage_bonus_percent <= 0 {
        return approximate_duel_win_probability(DuelOddsInput {
            player_hp: stats.player_hp,
            boss_hp: stats.boss_hp,
            player_hit: stats.player_hit,
            player_damage: stats.player_damage,
            boss_hit: stats.boss_hit,
            boss_damage: stats.boss_damage,
            critical_chance: stats.critical_chance,
            critical_bonus: stats.critical_bonus,
        });
    }
    if stats.player_hp <= 0 || stats.boss_hp <= 0 {
        return 0.0;
    }
    let hit = stats.player_hit.clamp(0.0, 1.0);
    let boss_hit = stats.boss_hit.clamp(0.0, 1.0);
    let critical = stats.critical_chance.clamp(0.0, 1.0);
    let mut odds = vec![vec![0.0; (stats.boss_hp + 1) as usize]; (stats.player_hp + 1) as usize];
    let denominator = 1.0 - (1.0 - hit) * (1.0 - boss_hit);
    for player_hp in 1..=stats.player_hp {
        for boss_hp in 1..=stats.boss_hp {
            let mut contribution = 0.0;
            let critical_outcomes = [
                (1.0 - critical, stats.player_damage),
                (critical, stats.player_damage + stats.critical_bonus),
            ];
            for (critical_probability, raw_damage) in critical_outcomes {
                let scaled = raw_damage.max(0) * player_damage_bonus_percent.max(0);
                let guaranteed = scaled / 100;
                let remainder_probability = f64::from(scaled % 100) / 100.0;
                let bonus_outcomes = [
                    (1.0 - remainder_probability, raw_damage + guaranteed),
                    (remainder_probability, raw_damage + guaranteed + 1),
                ];
                for (bonus_probability, damage) in bonus_outcomes {
                    let probability = hit * critical_probability * bonus_probability;
                    if probability == 0.0 {
                        continue;
                    }
                    let remaining_boss = boss_hp - damage.max(0);
                    if remaining_boss <= 0 {
                        contribution += probability;
                    } else {
                        contribution += probability
                            * (1.0 - boss_hit)
                            * odds[player_hp as usize][remaining_boss as usize];
                        if player_hp > stats.boss_damage {
                            contribution += probability
                                * boss_hit
                                * odds[(player_hp - stats.boss_damage) as usize]
                                    [remaining_boss as usize];
                        }
                    }
                }
            }
            if player_hp > stats.boss_damage {
                contribution += (1.0 - hit)
                    * boss_hit
                    * odds[(player_hp - stats.boss_damage) as usize][boss_hp as usize];
            }
            odds[player_hp as usize][boss_hp as usize] = if denominator > 0.0 {
                contribution / denominator
            } else {
                0.0
            };
        }
    }
    odds[stats.player_hp as usize][stats.boss_hp as usize].clamp(0.05, 0.95)
}

#[must_use]
pub fn effective_player_hit(raw_hit: f64, wager: i64, forced_no_wager_phase: bool) -> f64 {
    let adjusted = if wager == 0 && !forced_no_wager_phase {
        raw_hit * FREE_FIGHT_ACCURACY_MULTIPLIER
    } else {
        raw_hit
    };
    let floor = if wager > 0 {
        WAGERED_PLAYER_HIT_FLOOR
    } else {
        PLAYER_HIT_FLOOR
    };
    adjusted.clamp(floor, PLAYER_HIT_CEILING)
}

pub const GROTHAK_MECHANICS: [&str; 3] = [
    "grothak_earthquake",
    "grothak_crumble_wall",
    "grothak_bedrock_bellow",
];

#[derive(Clone, Debug, PartialEq)]
pub struct PausedDuel {
    pub mechanic_id: String,
    pub pet_assist: Option<PetAssist>,
    pub critical_chance: f64,
    pub critical_bonus: i32,
    pub gear_snapshot_ids: Vec<i64>,
    pub pending_phase_event: Option<String>,
}

#[must_use]
pub fn start_paused_duel<R: BossDuelRng>(
    mechanics: &[&str],
    pet_assist: Option<PetAssist>,
    critical_chance: f64,
    critical_bonus: i32,
    gear_snapshot_ids: Vec<i64>,
    rng: &mut R,
) -> PausedDuel {
    let selected = mechanics
        .get(rng.choose_index(mechanics.len()))
        .copied()
        .unwrap_or_default();
    PausedDuel {
        mechanic_id: selected.to_owned(),
        pet_assist,
        critical_chance,
        critical_bonus,
        gear_snapshot_ids,
        pending_phase_event: None,
    }
}

#[must_use]
pub fn resume_paused_duel(snapshot: &PausedDuel) -> PausedDuel {
    snapshot.clone()
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
pub struct VictoryInput {
    pub depth: i32,
    pub prestige: i32,
    pub risk: RiskTier,
    pub wager: i64,
    pub win_chance: f64,
    pub echo_applied: bool,
    pub drain_next_reward: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VictorySettlement {
    pub gross_base_reward: i64,
    pub scaled_base_reward: i64,
    pub wager_profit: i64,
    pub credited: i64,
    pub audit_jc_delta: i64,
}

#[must_use]
pub fn settle_victory(input: VictoryInput) -> VictorySettlement {
    let gross_base_reward = boss_base_reward(input.depth, input.prestige);
    let scaled_base_reward = scale_positive_dig_jc(gross_base_reward);
    let mut wager_profit = 0;
    if input.wager > 0 {
        let base = WagerMultiplier::new(boss_payout_multiplier(input.depth, input.risk))
            .expect("authored boss multiplier is valid");
        let chance = WinChance::new(input.win_chance).expect("validated win chance");
        let tapered = effective_wager_multiplier(base, chance);
        let boss_bonus = (input.prestige >= 4)
            .then(|| WagerMultiplier::new(1.5).expect("constant multiplier is valid"));
        let capped = settled_wager_multiplier(tapered, boss_bonus).value();
        wager_profit = (input.wager as f64 * (capped - 1.0)) as i64;
        if input.drain_next_reward {
            let drain = (input.wager as f64 * capped * 0.25).round() as i64;
            wager_profit = (wager_profit - drain).max(0);
        }
        if input.echo_applied {
            wager_profit = (wager_profit as f64 * ECHO_PROFIT_MULTIPLIER) as i64;
        }
    }
    let credited = scaled_base_reward + wager_profit.max(0);
    VictorySettlement {
        gross_base_reward,
        scaled_base_reward,
        wager_profit,
        credited,
        audit_jc_delta: credited,
    }
}

#[must_use]
pub fn scout_total_return_multiplier(input: VictoryInput) -> f64 {
    let base = WagerMultiplier::new(boss_payout_multiplier(input.depth, input.risk))
        .expect("authored boss multiplier is valid");
    let chance = WinChance::new(input.win_chance).expect("validated win chance");
    let tapered = effective_wager_multiplier(base, chance);
    let boss_bonus = (input.prestige >= 4)
        .then(|| WagerMultiplier::new(1.5).expect("constant multiplier is valid"));
    let live = settled_wager_multiplier(tapered, boss_bonus).value();
    if input.echo_applied {
        1.0 + (live - 1.0) * ECHO_PROFIT_MULTIPLIER
    } else {
        live
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BossEcho {
    pub killer_id: i64,
    pub weakened_until: i64,
}

impl BossEcho {
    #[must_use]
    pub const fn applies_to(self, player_id: i64, now: i64) -> bool {
        self.killer_id != player_id && now < self.weakened_until
    }

    #[must_use]
    pub const fn refreshed(killer_id: i64, now: i64) -> Self {
        Self {
            killer_id,
            weakened_until: now + ECHO_WINDOW_SECONDS,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LossInput {
    pub balance: i64,
    pub depth: i32,
    pub wager: i64,
    pub forced_no_wager_phase: bool,
    pub knockback_roll: i32,
    pub resolved_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LossSettlement {
    pub balance_after: i64,
    pub depth_after: i32,
    pub jc_delta: i64,
    pub knockback: i32,
    pub next_dig_at: i64,
}

#[must_use]
pub fn settle_loss(input: LossInput) -> LossSettlement {
    let requested_debit = if input.wager > 0 {
        input.wager
    } else if input.forced_no_wager_phase {
        0
    } else {
        BOSS_LOSS_REPAIR_BILL
    };
    let actual_debit = requested_debit.min(input.balance.max(0));
    let knockback = input
        .knockback_roll
        .clamp(BOSS_LOSS_KNOCKBACK_MIN, BOSS_LOSS_KNOCKBACK_MAX);
    LossSettlement {
        balance_after: input.balance - actual_debit,
        depth_after: (input.depth - knockback).max(0),
        jc_delta: -actual_debit,
        knockback,
        next_dig_at: input.resolved_at + BOSS_LOSS_EXTRA_COOLDOWN_SECONDS,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GearSlot {
    Weapon,
    Armor,
    Boots,
    Amulet,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GearPiece {
    pub id: i64,
    pub name: String,
    pub slot: GearSlot,
    pub durability: i32,
    pub equipped: bool,
}

fn tick_piece(piece: &mut GearPiece, ticks: i32, broken: &mut Vec<String>) {
    let before = piece.durability;
    piece.durability = (piece.durability - ticks).max(0);
    if before > 0 && piece.durability == 0 {
        broken.push(piece.name.clone());
    }
}

#[must_use]
pub fn cleanup_abandoned_duel(
    gear: &mut [GearPiece],
    stale_snapshot_ids: Option<&[i64]>,
) -> Vec<String> {
    let Some(snapshot_ids) = stale_snapshot_ids else {
        return Vec::new();
    };
    let ids: BTreeSet<i64> = snapshot_ids.iter().copied().collect();
    let legacy_fallback = ids.is_empty();
    let mut broken = Vec::new();
    for piece in gear {
        if (legacy_fallback && piece.equipped) || ids.contains(&piece.id) {
            tick_piece(piece, 1, &mut broken);
        }
    }
    broken
}

#[must_use]
pub fn tick_resolved_fight_gear(gear: &mut [GearPiece], won: bool) -> Vec<String> {
    let mut broken = Vec::new();
    for piece in gear.iter_mut().filter(|piece| piece.equipped) {
        let ticks =
            1 + i32::from(!won && piece.slot == GearSlot::Armor) * BOSS_LOSS_EXTRA_GEAR_TICKS;
        tick_piece(piece, ticks, &mut broken);
    }
    broken
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BossProgressEntry {
    pub boss_id: String,
    pub status: String,
    pub pending_phase_event: Option<String>,
}

impl BossProgressEntry {
    pub fn enter_next_phase<R: BossDuelRng>(&mut self, rng: &mut R) -> String {
        let event = PHASE_TRANSITION_EVENTS[rng.choose_index(PHASE_TRANSITION_EVENTS.len())];
        self.status = "phase1_defeated".to_owned();
        self.pending_phase_event = Some(event.id.to_owned());
        event.id.to_owned()
    }

    pub fn consume_transition(&mut self, stats: &mut DuelStats) -> Option<PhaseTransitionEvent> {
        let event = self
            .pending_phase_event
            .take()
            .and_then(|id| transition_event(&id))?;
        stats.player_hp = (stats.player_hp + event.player_hp_delta).max(1);
        stats.boss_hp = (stats.boss_hp + event.boss_hp_delta).max(1);
        stats.player_hit = (stats.player_hit + event.player_hit_offset).clamp(0.05, 0.90);
        stats.boss_hit = (stats.boss_hit + event.boss_hit_offset).clamp(0.05, 0.95);
        stats.player_damage = (stats.player_damage + event.player_damage_delta).max(1);
        stats.boss_damage = (stats.boss_damage + event.boss_damage_delta).max(1);
        Some(event)
    }
}

pub fn merge_fresh_progress_preserving_consumption(
    fresh: &mut BossProgressEntry,
    resolved: &BossProgressEntry,
) {
    if fresh.pending_phase_event.is_some() && resolved.pending_phase_event.is_none() {
        fresh.pending_phase_event = None;
    }
}

#[must_use]
pub fn milestone_bonus(max_depth: i32, crossed_depth: i32) -> i64 {
    if crossed_depth <= max_depth {
        return 0;
    }
    match crossed_depth {
        25 => 3,
        50 => 6,
        75 => 12,
        100 => 20,
        _ => 0,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatPointLedger {
    pub prestige_level: i32,
    pub awarded_boundaries: BTreeSet<i32>,
    pub stat_points: i32,
}

impl StatPointLedger {
    pub fn award_first_clear(&mut self, boundary: i32) -> bool {
        if !self.awarded_boundaries.insert(boundary) {
            return false;
        }
        self.stat_points = self.stat_points.max(5) + BOSS_STAT_POINT_BONUS;
        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistedDuelState {
    pub balance: i64,
    pub depth: i32,
    pub stat_points: i32,
    pub defeated_boundaries: BTreeSet<i32>,
    pub revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BossAuditRecord {
    pub won: bool,
    pub jc_delta: i64,
    pub depth_after: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtomicBossMutation {
    pub balance_delta: i64,
    pub depth_after: i32,
    pub defeated_boundary: Option<i32>,
    pub stat_point_delta: i32,
    pub audit: BossAuditRecord,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BossDuelStoreError {
    Conflict,
    MissingPlayer,
}

pub trait BossDuelRepositoryPort {
    fn load(&self, player_id: i64) -> Option<PersistedDuelState>;
    fn commit(
        &mut self,
        player_id: i64,
        expected_revision: u64,
        mutation: AtomicBossMutation,
    ) -> Result<PersistedDuelState, BossDuelStoreError>;
    fn audits(&self) -> &[BossAuditRecord];
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryBossDuelRepository {
    states: BTreeMap<i64, PersistedDuelState>,
    audit_log: Vec<BossAuditRecord>,
}

impl InMemoryBossDuelRepository {
    pub fn insert(&mut self, player_id: i64, state: PersistedDuelState) {
        self.states.insert(player_id, state);
    }
}

impl BossDuelRepositoryPort for InMemoryBossDuelRepository {
    fn load(&self, player_id: i64) -> Option<PersistedDuelState> {
        self.states.get(&player_id).cloned()
    }

    fn commit(
        &mut self,
        player_id: i64,
        expected_revision: u64,
        mutation: AtomicBossMutation,
    ) -> Result<PersistedDuelState, BossDuelStoreError> {
        let state = self
            .states
            .get_mut(&player_id)
            .ok_or(BossDuelStoreError::MissingPlayer)?;
        if state.revision != expected_revision {
            return Err(BossDuelStoreError::Conflict);
        }
        state.balance += mutation.balance_delta;
        state.depth = mutation.depth_after;
        if let Some(boundary) = mutation.defeated_boundary {
            state.defeated_boundaries.insert(boundary);
        }
        state.stat_points += mutation.stat_point_delta;
        state.revision += 1;
        self.audit_log.push(mutation.audit);
        Ok(state.clone())
    }

    fn audits(&self) -> &[BossAuditRecord] {
        &self.audit_log
    }
}

pub struct BossDuelSettlementService<R> {
    repository: R,
}

impl<R> BossDuelSettlementService<R>
where
    R: BossDuelRepositoryPort,
{
    #[must_use]
    pub const fn new(repository: R) -> Self {
        Self { repository }
    }

    pub fn settle_loss_atomic(
        &mut self,
        player_id: i64,
        input: LossInput,
    ) -> Result<PersistedDuelState, BossDuelStoreError> {
        let current = self
            .repository
            .load(player_id)
            .ok_or(BossDuelStoreError::MissingPlayer)?;
        let settlement = settle_loss(LossInput {
            balance: current.balance,
            depth: current.depth,
            ..input
        });
        self.repository.commit(
            player_id,
            current.revision,
            AtomicBossMutation {
                balance_delta: settlement.jc_delta,
                depth_after: settlement.depth_after,
                defeated_boundary: None,
                stat_point_delta: 0,
                audit: BossAuditRecord {
                    won: false,
                    jc_delta: settlement.jc_delta,
                    depth_after: settlement.depth_after,
                },
            },
        )
    }

    pub fn settle_victory_atomic(
        &mut self,
        player_id: i64,
        boundary: i32,
        settlement: VictorySettlement,
    ) -> Result<PersistedDuelState, BossDuelStoreError> {
        let current = self
            .repository
            .load(player_id)
            .ok_or(BossDuelStoreError::MissingPlayer)?;
        let first_clear = !current.defeated_boundaries.contains(&boundary);
        self.repository.commit(
            player_id,
            current.revision,
            AtomicBossMutation {
                balance_delta: settlement.credited,
                depth_after: boundary,
                defeated_boundary: Some(boundary),
                stat_point_delta: i32::from(first_clear) * BOSS_STAT_POINT_BONUS,
                audit: BossAuditRecord {
                    won: true,
                    jc_delta: settlement.credited,
                    depth_after: boundary,
                },
            },
        )
    }

    #[must_use]
    pub const fn repository(&self) -> &R {
        &self.repository
    }
}

#[must_use]
pub fn soften_line(starting_hp: i32, ending_hp: i32, maximum_hp: i32) -> Option<String> {
    (ending_hp < starting_hp).then(|| format!("Boss softened to {ending_hp}/{maximum_hp} HP"))
}

#[must_use]
pub fn echo_explanation() -> &'static str {
    "-25% max HP; 30% less wager profit"
}

#[cfg(test)]
#[path = "boss_duel/tests.rs"]
mod tests;
