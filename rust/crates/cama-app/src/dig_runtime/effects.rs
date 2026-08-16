use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DigWeatherEffects {
    pub cave_in_bonus: f64,
    pub cave_in_loss_bonus: i64,
    pub cave_in_loss_cap: Option<i64>,
    pub advance_bonus: i64,
    pub event_chance_multiplier: f64,
    pub luminosity_drain_multiplier: f64,
    pub jc_multiplier: f64,
    pub jc_bonus: i64,
    pub artifact_multiplier: f64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ActiveBuffEffects {
    pub(super) advance_bonus: i64,
    pub(super) cave_in_reduction: f64,
    pub(super) yield_multiplier: f64,
}

impl Default for ActiveBuffEffects {
    fn default() -> Self {
        Self {
            advance_bonus: 0,
            cave_in_reduction: 0.0,
            yield_multiplier: 1.0,
        }
    }
}

pub(super) fn active_buff_effects(raw: Option<&str>) -> Option<(ActiveBuffEffects, i64)> {
    let value = serde_json::from_str::<Value>(raw?).ok()?;
    let remaining = value.get("digs_remaining")?.as_i64()?;
    if remaining <= 0 {
        return None;
    }
    let effect = value.get("effect").or_else(|| value.get("effects"));
    let number_i64 = |key: &str| {
        effect
            .and_then(|effect| effect.get(key))
            .and_then(Value::as_i64)
    };
    let number_f64 = |key: &str| {
        effect
            .and_then(|effect| effect.get(key))
            .and_then(Value::as_f64)
    };
    Some((
        ActiveBuffEffects {
            advance_bonus: number_i64("advance_bonus").unwrap_or(0),
            cave_in_reduction: number_f64("cave_in_reduction").unwrap_or(0.0),
            yield_multiplier: number_f64("yield_multiplier").unwrap_or(1.0).max(0.0),
        },
        remaining,
    ))
}

pub(super) fn multiplier_millionths(multiplier: f64) -> i64 {
    if !multiplier.is_finite() || multiplier <= 0.0 {
        return 0;
    }
    (multiplier * DIG_YIELD_MULTIPLIER_SCALE as f64)
        .round()
        .clamp(0.0, i64::MAX as f64) as i64
}

pub(super) fn bonus_basis_points(bonus: f64) -> i64 {
    multiplier_millionths(1.0 + bonus) / (DIG_YIELD_MULTIPLIER_SCALE / DIG_REWARD_BASIS_POINTS)
}

pub(super) fn luminosity_jc_multiplier(luminosity: i64) -> f64 {
    if luminosity >= 26 {
        1.0
    } else if luminosity >= 1 {
        1.25
    } else {
        1.50
    }
}

pub(super) fn apply_mana_base_yield(
    base_jc: i64,
    effects: &ManaEffects,
    entropy: &mut impl LootEntropy,
) -> i64 {
    if base_jc <= 0 {
        return base_jc;
    }
    let mut modified = base_jc;
    let variance = effects.dig_yield_variance.max(0.0);
    if variance > 0.0 {
        let roll = entropy.unit();
        if roll < variance {
            modified = modified.saturating_mul(2);
        } else if roll < variance * 2.0 {
            modified = 0;
        }
    }
    if modified > 0 && effects.green_steady_bonus > 0 {
        modified = modified.saturating_add(effects.green_steady_bonus);
    }
    modified.max(0)
}

pub(super) fn proportional_mana_yield_tax(amount: i64, rate: f64) -> i64 {
    if amount <= 0 || !rate.is_finite() || rate <= 0.0 {
        return 0;
    }
    ((amount as f64 * rate) as i64).max(1).min(amount)
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ActiveCurseEffects {
    pub(super) advance_bonus: i64,
    pub(super) cave_in_bonus: f64,
    pub(super) cooldown_penalty: f64,
    pub(super) jc_bonus: i64,
    pub(super) luminosity_drain: i64,
}

pub(super) fn active_curse_effects(raw: Option<&str>) -> Option<(ActiveCurseEffects, i64)> {
    let value: Value = serde_json::from_str(raw?).ok()?;
    let remaining = value.get("digs_remaining")?.as_i64()?;
    if remaining <= 0 {
        return None;
    }
    let effect = value.get("effect").or_else(|| value.get("effects"));
    let number_i64 = |key: &str| {
        effect
            .and_then(|effect| effect.get(key))
            .and_then(Value::as_i64)
    };
    let number_f64 = |key: &str| {
        effect
            .and_then(|effect| effect.get(key))
            .and_then(Value::as_f64)
    };
    Some((
        ActiveCurseEffects {
            advance_bonus: number_i64("advance_bonus").unwrap_or(0),
            cave_in_bonus: number_f64("cave_in_bonus").unwrap_or(0.0),
            cooldown_penalty: number_f64("cooldown_penalty").unwrap_or(0.0),
            jc_bonus: number_i64("jc_bonus").unwrap_or(0),
            luminosity_drain: number_i64("luminosity_drain").unwrap_or(0),
        },
        remaining,
    ))
}

impl DigWeatherEffects {
    pub(super) const fn neutral() -> Self {
        Self {
            cave_in_bonus: 0.0,
            cave_in_loss_bonus: 0,
            cave_in_loss_cap: None,
            advance_bonus: 0,
            event_chance_multiplier: 0.0,
            luminosity_drain_multiplier: 0.0,
            jc_multiplier: 0.0,
            jc_bonus: 0,
            artifact_multiplier: 0.0,
        }
    }
}

pub(super) fn weather_effects(weather_id: &str) -> DigWeatherEffects {
    // These are the authored Python weather modifiers. Keeping the mapping
    // keyed by the persisted id makes unknown/migrated rows fail closed to a
    // neutral effect while remaining visible in the outcome detail.
    match weather_id {
        "earthworm_migration" => DigWeatherEffects {
            advance_bonus: 1,
            jc_bonus: -1,
            ..DigWeatherEffects::neutral()
        },
        "mudslide_warning" => DigWeatherEffects {
            cave_in_bonus: 0.10,
            cave_in_loss_cap: Some(3),
            ..DigWeatherEffects::neutral()
        },
        "root_overgrowth" => DigWeatherEffects {
            advance_bonus: -1,
            artifact_multiplier: 2.0,
            ..DigWeatherEffects::neutral()
        },
        "fossil_rush" => DigWeatherEffects {
            artifact_multiplier: 2.0,
            ..DigWeatherEffects::neutral()
        },
        "seismic_tremors" => DigWeatherEffects {
            cave_in_bonus: 0.08,
            event_chance_multiplier: 0.50,
            ..DigWeatherEffects::neutral()
        },
        "mineral_vein" => DigWeatherEffects {
            jc_bonus: 2,
            ..DigWeatherEffects::neutral()
        },
        "crystal_resonance" => DigWeatherEffects {
            jc_multiplier: -0.25,
            ..DigWeatherEffects::neutral()
        },
        "prismatic_surge" => DigWeatherEffects {
            event_chance_multiplier: 1.0,
            jc_bonus: 3,
            ..DigWeatherEffects::neutral()
        },
        "shatter_warning" => DigWeatherEffects {
            cave_in_bonus: 0.12,
            jc_bonus: 3,
            ..DigWeatherEffects::neutral()
        },
        "eruption" => DigWeatherEffects {
            cave_in_bonus: 0.12,
            jc_multiplier: 0.75,
            ..DigWeatherEffects::neutral()
        },
        "cooling_period" => DigWeatherEffects {
            cave_in_bonus: -0.10,
            jc_multiplier: -0.25,
            ..DigWeatherEffects::neutral()
        },
        "lava_bloom" => DigWeatherEffects {
            artifact_multiplier: 1.5,
            luminosity_drain_multiplier: 0.50,
            ..DigWeatherEffects::neutral()
        },
        "void_tide" => DigWeatherEffects {
            cave_in_loss_bonus: 2,
            ..DigWeatherEffects::neutral()
        },
        "whisper_storm" => DigWeatherEffects {
            event_chance_multiplier: 1.0,
            ..DigWeatherEffects::neutral()
        },
        "deep_calm" => DigWeatherEffects {
            cave_in_bonus: -0.12,
            event_chance_multiplier: -0.50,
            ..DigWeatherEffects::neutral()
        },
        "spore_bloom" => DigWeatherEffects {
            advance_bonus: 2,
            luminosity_drain_multiplier: 1.0,
            ..DigWeatherEffects::neutral()
        },
        "mycelium_pulse" => DigWeatherEffects {
            jc_multiplier: 0.50,
            event_chance_multiplier: 0.25,
            ..DigWeatherEffects::neutral()
        },
        "fungal_frenzy" => DigWeatherEffects {
            event_chance_multiplier: 2.0,
            cave_in_bonus: 0.08,
            ..DigWeatherEffects::neutral()
        },
        "time_dilation" => DigWeatherEffects {
            jc_multiplier: 1.0,
            advance_bonus: -1,
            ..DigWeatherEffects::neutral()
        },
        "frozen_stillness" => DigWeatherEffects {
            cave_in_bonus: -1.0,
            event_chance_multiplier: -1.0,
            ..DigWeatherEffects::neutral()
        },
        "temporal_storm" => DigWeatherEffects {
            cave_in_bonus: 0.15,
            jc_multiplier: 0.75,
            event_chance_multiplier: 0.50,
            ..DigWeatherEffects::neutral()
        },
        "hollow_breathes" => DigWeatherEffects {
            jc_multiplier: 0.50,
            cave_in_bonus: 0.10,
            event_chance_multiplier: 0.50,
            ..DigWeatherEffects::neutral()
        },
        "void_harvest" => DigWeatherEffects {
            artifact_multiplier: 3.0,
            cave_in_bonus: 0.15,
            ..DigWeatherEffects::neutral()
        },
        "deep_silence" => DigWeatherEffects {
            cave_in_bonus: -0.15,
            jc_multiplier: -0.50,
            ..DigWeatherEffects::neutral()
        },
        _ => DigWeatherEffects::neutral(),
    }
}

pub(super) fn event_chance_factor(
    ascension_delta: f64,
    weather_delta: f64,
    route_delta: f64,
) -> f64 {
    (1.0 + ascension_delta) * (1.0 + weather_delta) * (1.0 + route_delta)
}

pub(super) fn weather_code(weather_id: Option<&str>) -> Option<&str> {
    // Python forwards the persisted weather id verbatim (lowercased by its
    // repository). Authored ids such as `temporal_storm` and
    // `whisper_storm` are not the special generic `storm` code and must not
    // activate Stormcaller or Blue-Mana combo policy.
    weather_id
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct DigGearEffects {
    pub(super) advance_bonus: i64,
    pub(super) cave_in_reduction: f64,
    pub(super) loot_bonus: i64,
}

pub(super) fn active_pickaxe_tier(gear: &[DigRuntimeGear], tunnel: &DigRuntimeTunnel) -> i64 {
    if let Some(weapon) = gear
        .iter()
        .find(|piece| piece.equipped && piece.slot == "weapon")
    {
        return if weapon.durability > 0 {
            weapon.tier.max(0)
        } else {
            0
        };
    }
    tunnel.pickaxe_tier.max(0)
}

pub(super) fn gear_effects(gear: &[DigRuntimeGear], tunnel: &DigRuntimeTunnel) -> DigGearEffects {
    let Some(weapon) = gear
        .iter()
        .find(|piece| piece.equipped && piece.slot == "weapon")
    else {
        return WEAPON_TIERS
            .get(usize::try_from(tunnel.pickaxe_tier.max(0)).unwrap_or_default())
            .map_or_else(DigGearEffects::default, |definition| DigGearEffects {
                advance_bonus: i64::from(definition.advance_bonus),
                cave_in_reduction: definition.cave_in_reduction,
                loot_bonus: i64::from(definition.loot_bonus),
            });
    };
    if weapon.durability <= 0 {
        return DigGearEffects::default();
    }
    if let Some(unique) = weapon.item_id.as_deref().and_then(unique_gear) {
        return DigGearEffects {
            advance_bonus: i64::from(unique.advance_bonus),
            cave_in_reduction: unique.cave_in_reduction,
            loot_bonus: i64::from(unique.loot_bonus),
        };
    }
    WEAPON_TIERS
        .get(usize::try_from(weapon.tier.max(0)).unwrap_or_default())
        .map_or_else(DigGearEffects::default, |definition| DigGearEffects {
            advance_bonus: i64::from(definition.advance_bonus),
            cave_in_reduction: definition.cave_in_reduction,
            loot_bonus: i64::from(definition.loot_bonus),
        })
}

fn runtime_gear_name(piece: &DigRuntimeGear) -> String {
    if let Some(item_id) = piece.item_id.as_deref()
        && let Some(definition) = unique_gear(item_id)
    {
        return definition.name.to_owned();
    }
    let tier = usize::try_from(piece.tier.max(0)).unwrap_or_default();
    let name = match piece.slot.as_str() {
        "armor" => ARMOR_TIERS.get(tier).map(|definition| definition.name),
        "boots" => BOOTS_TIERS.get(tier).map(|definition| definition.name),
        "amulet" => AMULET_TIERS.get(tier).map(|definition| definition.name),
        "weapon" => WEAPON_TIERS.get(tier).map(|definition| definition.name),
        _ => None,
    };
    name.map_or_else(
        || format!("{} tier {}", piece.slot, piece.tier),
        str::to_owned,
    )
}

/// Tick equipped gear for a cave-in and report only newly broken pieces.
///
/// Broken pieces remain equipped, but are not applicable to a later
/// `gear_nick` consequence.  This mirrors the repository's durability
/// contract while keeping the Dig runtime's staged snapshot authoritative.
pub fn apply_cave_in_gear_ticks(gear: &mut [DigRuntimeGear], ticks: u32) -> Vec<String> {
    let mut broken = Vec::new();
    for _ in 0..ticks {
        for piece in gear.iter_mut().filter(|piece| piece.equipped) {
            if piece.durability == 1 {
                let name = runtime_gear_name(piece);
                if !broken.contains(&name) {
                    broken.push(name);
                }
            }
            piece.durability = piece.durability.saturating_sub(1).max(0);
        }
    }
    broken
}

/// Apply the catastrophic depth rule after the ordinary block-loss roll.
/// Insurance covers only the milestone rollback; the ordinary loss remains in
/// place, exactly as in the Python service.
#[must_use]
pub fn catastrophic_cave_in_depth(
    depth_before: i64,
    block_loss: i64,
    block_loss_cap: Option<i64>,
    insured: bool,
) -> (i64, bool, i64) {
    let ordinary_depth = (depth_before - block_loss.max(0)).max(0);
    if insured {
        return (ordinary_depth, true, block_loss.max(0));
    }
    let milestone = ((depth_before.saturating_sub(1).max(0))
        / i64::from(CAVE_IN_CATASTROPHIC_MILESTONE_STEP))
        * i64::from(CAVE_IN_CATASTROPHIC_MILESTONE_STEP);
    let capped_depth = block_loss_cap
        .map(|cap| (depth_before - cap.max(0)).max(0))
        .unwrap_or(milestone);
    let depth_after = milestone.max(capped_depth);
    (
        depth_after,
        false,
        (depth_before - depth_after).max(block_loss.max(0)),
    )
}

pub(super) struct CaveInLootRng<'a>(pub(super) &'a mut SeededLootEntropy);

impl CaveInRng for CaveInLootRng<'_> {
    fn random_unit(&mut self) -> f64 {
        self.0.unit()
    }

    fn random_inclusive(&mut self, lower: u32, upper: u32) -> u32 {
        let lower = i64::from(lower);
        let upper = i64::from(upper);
        u32::try_from(self.0.advance(lower, upper)).unwrap_or(lower as u32)
    }
}

pub(super) struct LootRelicEntropy<'a>(pub(super) &'a mut SeededLootEntropy);

impl RelicEntropy for LootRelicEntropy<'_> {
    fn unit(&mut self) -> f64 {
        self.0.unit()
    }
}

/// Adapt the live Dig entropy stream to the pure prestige-four artifact policy.
///
/// The adapter is intentionally local to the runtime transaction: corruption,
/// cave consequences, JC, artifact, and event selection all advance the same
/// request-local stream, while the pure policy remains persistence-free.
pub(super) struct DigPrestige4Entropy<'a>(pub(super) &'a mut SeededLootEntropy);

impl Prestige4Entropy for DigPrestige4Entropy<'_> {
    fn unit(&mut self) -> f64 {
        self.0.unit()
    }

    fn choose_index(&mut self, upper_bound: usize) -> usize {
        self.0.choose_index(upper_bound)
    }
}
