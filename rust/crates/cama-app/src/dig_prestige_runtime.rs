//! Canonical prestige-picker and prestige-perk policy for the Dig runtime.
//!
//! The Discord provider only projects this policy. Stable picker entropy,
//! stack eligibility, and stateful settlement live here; canonical perk
//! aggregation, event-risk detection, and Reading-the-Stone policy are
//! re-exported from `dig_tunnels` so preview/restart behavior cannot diverge
//! between a component callback and the atomic prestige settlement.

use std::path::{Path, PathBuf};

use cama_db::dig_prestige_runtime::{
    AtomicDigPrestigeSettlement, DigPrestigeKey, DigPrestigeRuntimeRepository,
    DigPrestigeRuntimeRepositoryError, DigPrestigeSettlementOutcome, DigPrestigeSnapshot,
};
use cama_domain::dig_economy::{DIG_POSITIVE_JC_MULTIPLIER, scale_positive_dig_jc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha512};
use thiserror::Error;

use crate::dig_bosses::{BOSS_BOUNDARIES, PINNACLE_DEPTH};
use crate::dig_loot::{Rarity, artifact_catalog};
use crate::dig_runtime::DigRuntimeConfig;
use crate::dig_tunnels::{
    MAX_PRESTIGE, MUTATIONS, MutationDefinition, PRESTIGE_GROSS_JC, PRESTIGE_PERK_STACK_CAP,
    PRESTIGE_PERKS, RelicGrant, RelicRarity, prestige_mutation_seed,
};
use crate::economy_event_sqlite::{SqliteEconomyEventError, SqliteEconomyEventService};
use crate::python_random::PythonRandom;

pub use crate::dig_tunnels::{
    READING_STONE_DESPERATE_HINTS, READING_STONE_RISKY_HINTS, READING_STONE_SAFE_HINTS,
    ReadingStoneDirection, aggregate_prestige_perk_effects, event_has_risk, prestige_perk_effects,
    reading_the_stone_direction, reading_the_stone_hint,
};

pub const PRESTIGE_PICK_COUNT: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrestigePerkChoice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PrestigeMutationChoice {
    pub id: String,
    pub name: String,
    pub description: String,
    pub positive: bool,
}

impl From<MutationDefinition> for PrestigeMutationChoice {
    fn from(mutation: MutationDefinition) -> Self {
        Self {
            id: mutation.id.to_owned(),
            name: mutation.name.to_owned(),
            description: mutation.description.to_owned(),
            positive: mutation.positive,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrestigeMutationPreview {
    pub forced: PrestigeMutationChoice,
    pub choices: Vec<PrestigeMutationChoice>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PrestigeMutationResult {
    pub forced: PrestigeMutationChoice,
    pub chosen: Option<PrestigeMutationChoice>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrestigeAscensionUnlock {
    pub name: &'static str,
    pub penalty: &'static str,
    pub reward: &'static str,
    pub gameplay: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigPrestigePreview {
    pub can_prestige: bool,
    pub reason: Option<String>,
    pub current_level: i32,
    pub target_level: i32,
    pub run_score: i64,
    pub available_perks: Vec<String>,
    pub offered_perks: Vec<PrestigePerkChoice>,
    pub mutation: Option<PrestigeMutationPreview>,
    pub ascension_unlock: Option<PrestigeAscensionUnlock>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigPrestigeRequest<'a> {
    pub discord_id: i64,
    pub guild_id: i64,
    pub perk_choice: &'a str,
    pub mutation_choice: Option<&'a str>,
    pub now: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigPrestigeGrant {
    pub jc: i64,
    pub relic: Option<RelicGrant>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigPrestigeResult {
    pub prestige_level: i32,
    pub perk_chosen: String,
    pub perk_name: String,
    pub perks: Vec<String>,
    pub run_score: i64,
    pub best_run_score: i64,
    pub total_prestige_score: i64,
    pub ascension_unlocked: Option<PrestigeAscensionUnlock>,
    pub mutations: Option<PrestigeMutationResult>,
    pub prestige_grant: DigPrestigeGrant,
    pub balance_after: i64,
    pub relic_row_id: Option<i64>,
    pub audit_action_id: i64,
}

#[derive(Debug, Error)]
pub enum DigPrestigeRuntimeError {
    #[error("{0}")]
    Unavailable(String),
    #[error("Invalid perk. Choose one of the perks shown for this prestige.")]
    InvalidPerk,
    #[error("You've maxed that perk.")]
    PerkAtCap,
    #[error("Invalid mutation. Choose one of the mutations shown for this prestige.")]
    InvalidMutation,
    #[error("Your prestige was already processed.")]
    Conflict,
    #[error("The registered player row is missing.")]
    MissingPlayer,
    #[error("Stored prestige state is invalid: {0}")]
    InvalidState(String),
    #[error(transparent)]
    Repository(#[from] DigPrestigeRuntimeRepositoryError),
    #[error(transparent)]
    Economy(#[from] SqliteEconomyEventError),
}

pub trait PrestigeRelicEntropy {
    fn choose_index(&mut self, upper_bound: usize) -> usize;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessPrestigeRelicEntropy;

impl PrestigeRelicEntropy for ProcessPrestigeRelicEntropy {
    fn choose_index(&mut self, upper_bound: usize) -> usize {
        fastrand::usize(..upper_bound)
    }
}

#[derive(Clone, Debug)]
pub struct DigPrestigeRuntimeService<E = ProcessPrestigeRelicEntropy> {
    path: PathBuf,
    repository: DigPrestigeRuntimeRepository,
    config: DigRuntimeConfig,
    relic_entropy: E,
}

impl DigPrestigeRuntimeService<ProcessPrestigeRelicEntropy> {
    #[must_use]
    pub fn sqlite(path: impl AsRef<Path>) -> Self {
        Self::sqlite_with_config(path, DigRuntimeConfig::default())
    }

    #[must_use]
    pub fn sqlite_with_config(path: impl AsRef<Path>, config: DigRuntimeConfig) -> Self {
        Self::with_entropy(path, config, ProcessPrestigeRelicEntropy)
    }
}

impl<E> DigPrestigeRuntimeService<E>
where
    E: PrestigeRelicEntropy,
{
    #[must_use]
    pub fn with_entropy(
        path: impl AsRef<Path>,
        config: DigRuntimeConfig,
        relic_entropy: E,
    ) -> Self {
        let path = path.as_ref().to_path_buf();
        Self {
            repository: DigPrestigeRuntimeRepository::new(&path),
            path,
            config,
            relic_entropy,
        }
    }

    pub fn preview(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<DigPrestigePreview, DigPrestigeRuntimeError> {
        let key = DigPrestigeKey {
            discord_id,
            guild_id,
        };
        let Some(snapshot) = self.repository.snapshot(key)? else {
            return Ok(DigPrestigePreview {
                can_prestige: false,
                reason: Some("No tunnel.".to_owned()),
                current_level: 0,
                target_level: 1,
                run_score: 0,
                available_perks: Vec::new(),
                offered_perks: Vec::new(),
                mutation: None,
                ascension_unlock: ascension_unlock(1),
            });
        };
        preview_from_snapshot(&snapshot)
    }

    pub fn prestige(
        &mut self,
        request: DigPrestigeRequest<'_>,
    ) -> Result<DigPrestigeResult, DigPrestigeRuntimeError> {
        let key = DigPrestigeKey {
            discord_id: request.discord_id,
            guild_id: request.guild_id,
        };
        let snapshot = self
            .repository
            .snapshot(key)?
            .ok_or_else(|| DigPrestigeRuntimeError::Unavailable("No tunnel.".to_owned()))?;
        let preview = preview_from_snapshot(&snapshot)?;
        if !preview.can_prestige {
            return Err(DigPrestigeRuntimeError::Unavailable(
                preview
                    .reason
                    .unwrap_or_else(|| "Cannot prestige.".to_owned()),
            ));
        }
        if !PRESTIGE_PERKS.contains(&request.perk_choice) {
            return Err(DigPrestigeRuntimeError::InvalidPerk);
        }
        let mut perks = prestige_perks(&snapshot)?;
        if perks
            .iter()
            .filter(|perk| perk.as_str() == request.perk_choice)
            .count()
            >= PRESTIGE_PERK_STACK_CAP
        {
            return Err(DigPrestigeRuntimeError::PerkAtCap);
        }
        if !preview
            .offered_perks
            .iter()
            .any(|perk| perk.id == request.perk_choice)
        {
            return Err(DigPrestigeRuntimeError::InvalidPerk);
        }

        let target_level = preview.target_level;
        let (mutations_json, mutation_result) = prestige_mutations(
            request.discord_id,
            request.guild_id,
            target_level,
            request.mutation_choice,
        )?;
        perks.push(request.perk_choice.to_owned());
        let perks_json = serde_json::to_string(&perks)
            .map_err(|error| DigPrestigeRuntimeError::InvalidState(error.to_string()))?;
        let best_run_score = snapshot.best_run_score.max(preview.run_score);
        let total_prestige_score = snapshot
            .total_prestige_score
            .saturating_add(preview.run_score);

        let economy = SqliteEconomyEventService::new(&self.path, self.config.economy_event.clone());
        let event_adjusted =
            economy.adjust_reward_at(request.guild_id, PRESTIGE_GROSS_JC, request.now)?;
        let jc_grant = scale_positive_dig_jc(event_adjusted);
        let eligible_relics = artifact_catalog()
            .into_iter()
            .filter(|artifact| {
                artifact.is_relic
                    && !artifact.trophy
                    && matches!(artifact.rarity, Rarity::Rare | Rarity::Legendary)
                    && !snapshot.owned_artifact_ids.contains(artifact.id)
            })
            .map(|artifact| RelicGrant {
                id: artifact.id,
                name: artifact.name,
                rarity: match artifact.rarity {
                    Rarity::Legendary => RelicRarity::Legendary,
                    _ => RelicRarity::Rare,
                },
            })
            .collect::<Vec<_>>();
        let relic = (!eligible_relics.is_empty()).then(|| {
            let index = self.relic_entropy.choose_index(eligible_relics.len());
            eligible_relics[index % eligible_relics.len()]
        });
        let boss_progress_json = active_boss_progress_json()?;
        let stat_boss_awards_json = serde_json::json!({
            "prestige_level": target_level,
            "awards": [],
        })
        .to_string();

        let outcome = self.repository.atomic_settle(AtomicDigPrestigeSettlement {
            expected: &snapshot,
            next_prestige_level: i64::from(target_level),
            prestige_perks_json: &perks_json,
            boss_progress_json: &boss_progress_json,
            mutations_json: mutations_json.as_deref(),
            stat_boss_awards_json: &stat_boss_awards_json,
            best_run_score,
            total_prestige_score,
            balance_delta: jc_grant,
            relic_artifact_id: relic.map(|relic| relic.id),
            created_at: request.now,
        })?;
        let receipt = match outcome {
            DigPrestigeSettlementOutcome::Applied(receipt) => receipt,
            DigPrestigeSettlementOutcome::Conflict => {
                return Err(DigPrestigeRuntimeError::Conflict);
            }
            DigPrestigeSettlementOutcome::MissingTunnel => {
                return Err(DigPrestigeRuntimeError::Unavailable(
                    "No tunnel.".to_owned(),
                ));
            }
            DigPrestigeSettlementOutcome::MissingPlayer => {
                return Err(DigPrestigeRuntimeError::MissingPlayer);
            }
        };

        let audit_detail = serde_json::json!({
            "level": target_level,
            "perk": request.perk_choice,
            "run_score": preview.run_score,
            "mutations": mutation_result,
            "gross_jc": PRESTIGE_GROSS_JC,
            "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
        })
        .to_string();
        let audit_action_id =
            self.repository
                .append_audit(key, jc_grant, &audit_detail, request.now)?;

        Ok(DigPrestigeResult {
            prestige_level: target_level,
            perk_chosen: request.perk_choice.to_owned(),
            perk_name: prestige_perk_display_name(request.perk_choice),
            perks,
            run_score: preview.run_score,
            best_run_score,
            total_prestige_score,
            ascension_unlocked: preview.ascension_unlock,
            mutations: mutation_result,
            prestige_grant: DigPrestigeGrant {
                jc: jc_grant,
                relic,
            },
            balance_after: receipt.balance_after,
            relic_row_id: receipt.relic_row_id,
            audit_action_id,
        })
    }
}

fn preview_from_snapshot(
    snapshot: &DigPrestigeSnapshot,
) -> Result<DigPrestigePreview, DigPrestigeRuntimeError> {
    let current_level = i32::try_from(snapshot.prestige_level).map_err(|_| {
        DigPrestigeRuntimeError::InvalidState("prestige level is outside i32".to_owned())
    })?;
    let target_level = current_level.saturating_add(1);
    let boss_progress: Value = serde_json::from_str(&snapshot.boss_progress_json)
        .unwrap_or_else(|_| serde_json::json!({}));
    let remaining = BOSS_BOUNDARIES
        .into_iter()
        .filter(|boundary| {
            boss_progress
                .get(boundary.to_string())
                .is_none_or(|state| !boss_state_is_defeated(state))
        })
        .collect::<Vec<_>>();
    let pinnacle_defeated = boss_progress
        .get(PINNACLE_DEPTH.to_string())
        .is_some_and(boss_state_is_defeated);
    let reason = if !remaining.is_empty() {
        Some(format!(
            "Bosses remaining: {}",
            remaining
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    } else if !pinnacle_defeated {
        Some("Something stirs deeper still — descend further.".to_owned())
    } else if current_level >= MAX_PRESTIGE {
        Some(format!("Already at max prestige ({MAX_PRESTIGE})."))
    } else {
        None
    };
    let can_prestige = reason.is_none();
    let perks = prestige_perks(snapshot)?;
    let eligible = eligible_prestige_perks(&perks);
    let offered = prestige_perk_choices(&eligible, snapshot.key.discord_id, target_level)
        .into_iter()
        .map(|id| PrestigePerkChoice {
            name: prestige_perk_display_name(&id),
            id,
        })
        .collect();
    let mutation = can_prestige
        .then(|| mutation_preview(snapshot.key, target_level))
        .flatten();
    Ok(DigPrestigePreview {
        can_prestige,
        reason,
        current_level,
        target_level,
        run_score: if can_prestige {
            calculate_run_score(snapshot, &boss_progress)
        } else {
            0
        },
        available_perks: eligible.into_iter().map(str::to_owned).collect(),
        offered_perks: offered,
        mutation,
        ascension_unlock: ascension_unlock(target_level),
    })
}

fn prestige_perks(snapshot: &DigPrestigeSnapshot) -> Result<Vec<String>, DigPrestigeRuntimeError> {
    match serde_json::from_str::<Vec<String>>(&snapshot.prestige_perks_json) {
        Ok(perks) => Ok(perks),
        // Python's `_get_prestige_perks` treats malformed/legacy values as an
        // empty list instead of making the tunnel unusable.
        Err(_) => Ok(Vec::new()),
    }
}

fn boss_state_is_defeated(state: &Value) -> bool {
    state.as_str() == Some("defeated")
        || state.get("status").and_then(Value::as_str) == Some("defeated")
}

fn calculate_run_score(snapshot: &DigPrestigeSnapshot, boss_progress: &Value) -> i64 {
    let bosses_defeated = boss_progress.as_object().map_or(0, |progress| {
        progress
            .values()
            .filter(|state| boss_state_is_defeated(state))
            .count() as i64
    });
    let base = snapshot.depth
        + bosses_defeated * 50
        + (snapshot.current_run_jc as f64 * 0.5) as i64
        + snapshot.current_run_events * 10;
    let mut multiplier = 1.0 + snapshot.prestige_level as f64 * 0.1;
    if snapshot.prestige_level >= 10 {
        multiplier += 2.0;
    }
    (base as f64 * multiplier) as i64
}

fn mutation_preview(key: DigPrestigeKey, target_level: i32) -> Option<PrestigeMutationPreview> {
    if target_level < 8 {
        return None;
    }
    let seed = prestige_mutation_seed(key.discord_id, key.guild_id, target_level);
    let roll = python_seeded_mutation_roll(&seed);
    Some(PrestigeMutationPreview {
        forced: roll.forced.into(),
        choices: roll.choices.into_iter().map(Into::into).collect(),
    })
}

fn python_seeded_mutation_roll(seed: &str) -> crate::dig_tunnels::MutationRoll {
    // Python's `random.Random(str)` version-2 seed is the concatenation of
    // the UTF-8 seed bytes and SHA-512(seed), interpreted as one big-endian
    // integer. `_random.Random.seed` then feeds its little-endian 32-bit words
    // to MT19937's init-by-array routine. Reproducing that path makes preview
    // and settlement byte-for-byte compatible with `random.shuffle(pool)`.
    let digest = Sha512::digest(seed.as_bytes());
    let mut seed_material = seed.as_bytes().to_vec();
    seed_material.extend_from_slice(&digest);
    let mut words = seed_material
        .rchunks(4)
        .map(|chunk| {
            chunk
                .iter()
                .fold(0_u32, |value, byte| (value << 8) | u32::from(*byte))
        })
        .collect::<Vec<_>>();
    while words.last() == Some(&0) {
        words.pop();
    }
    if words.is_empty() {
        words.push(0);
    }
    let mut random = PythonIntegerRandom::from_words(&words);
    let mut pool = MUTATIONS;
    // CPython `random.shuffle`: reverse Fisher-Yates using `_randbelow(i+1)`.
    for index in (1..pool.len()).rev() {
        let replacement = random.rand_below(index + 1);
        pool.swap(index, replacement);
    }
    crate::dig_tunnels::MutationRoll {
        forced: pool[0],
        choices: pool[1..4].to_vec(),
    }
}

fn prestige_mutations(
    discord_id: i64,
    guild_id: i64,
    target_level: i32,
    mutation_choice: Option<&str>,
) -> Result<(Option<String>, Option<PrestigeMutationResult>), DigPrestigeRuntimeError> {
    let Some(preview) = mutation_preview(
        DigPrestigeKey {
            discord_id,
            guild_id,
        },
        target_level,
    ) else {
        if mutation_choice.is_some() {
            return Err(DigPrestigeRuntimeError::InvalidMutation);
        }
        return Ok((None, None));
    };
    let chosen = match mutation_choice {
        Some(choice) => preview
            .choices
            .iter()
            .find(|mutation| mutation.id == choice)
            .cloned()
            .ok_or(DigPrestigeRuntimeError::InvalidMutation)?,
        None => preview
            .choices
            .first()
            .cloned()
            .ok_or(DigPrestigeRuntimeError::InvalidMutation)?,
    };
    let active = [mutation_json(&preview.forced), mutation_json(&chosen)];
    let result = PrestigeMutationResult {
        forced: preview.forced,
        chosen: Some(chosen),
    };
    Ok((Some(Value::Array(active.into()).to_string()), Some(result)))
}

fn mutation_json(mutation: &PrestigeMutationChoice) -> Value {
    serde_json::json!({
        "id": mutation.id,
        "name": mutation.name,
        "description": mutation.description,
        "positive": mutation.positive,
    })
}

fn active_boss_progress_json() -> Result<String, DigPrestigeRuntimeError> {
    let progress = BOSS_BOUNDARIES
        .into_iter()
        .map(|boundary| (boundary.to_string(), Value::String("active".to_owned())))
        .collect::<serde_json::Map<_, _>>();
    serde_json::to_string(&progress)
        .map_err(|error| DigPrestigeRuntimeError::InvalidState(error.to_string()))
}

#[must_use]
pub const fn ascension_unlock(level: i32) -> Option<PrestigeAscensionUnlock> {
    let (name, penalty, reward, gameplay) = match level {
        1 => (
            "Dense Stone",
            "Stone is denser but richer",
            "JC loot +18%",
            false,
        ),
        2 => (
            "Unstable Ground",
            "Cave-in chance +3%, dig JC -3%",
            "Event chance +20%",
            false,
        ),
        3 => (
            "Hungry Darkness",
            "Luminosity drains 25% faster, dig JC -2%",
            "Rare events 50% more common",
            false,
        ),
        4 => (
            "Boss Rage",
            "Bosses fight harder the longer you delve",
            "Boss payouts +50%",
            false,
        ),
        5 => (
            "Erosion",
            "Decay rate +50%",
            "Milestone rewards +50%",
            false,
        ),
        6 => (
            "Corruption",
            "Each dig rolls a random micro-modifier",
            "Artifact find rate doubled",
            true,
        ),
        7 => (
            "Event Chains",
            "Events can chain (same or higher rarity)",
            "Chained events give 1.5x JC",
            true,
        ),
        8 => (
            "Mutations",
            "1 forced random mutation per prestige run",
            "Choose 1 mutation from 3 (may be positive)",
            true,
        ),
        9 => (
            "Cruel Echoes",
            "Safe event options now have 10% failure chance",
            "Legendary events 3x more common",
            true,
        ),
        10 => (
            "The Endless",
            "Paid dig costs +50%",
            "Score multiplier 2x",
            false,
        ),
        _ => return None,
    };
    Some(PrestigeAscensionUnlock {
        name,
        penalty,
        reward,
        gameplay,
    })
}

#[must_use]
pub fn prestige_perk_display_name(perk_id: &str) -> String {
    if perk_id == "loot_multiplier" {
        return "Loot Bonus".to_owned();
    }
    perk_id
        .split('_')
        .map(titlecase_ascii_word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn titlecase_ascii_word(word: &str) -> String {
    let mut characters = word.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    first.to_uppercase().chain(characters).collect()
}

#[must_use]
pub fn eligible_prestige_perks(owned: &[String]) -> Vec<&'static str> {
    PRESTIGE_PERKS
        .into_iter()
        .filter(|candidate| {
            owned
                .iter()
                .filter(|perk| perk.as_str() == *candidate)
                .count()
                < PRESTIGE_PERK_STACK_CAP
        })
        .collect()
}

/// Match `struct.unpack_from("<Q", sha256(f"{user}:{level}").digest())[0]`.
#[must_use]
pub fn prestige_picker_seed(discord_id: i64, target_level: i32) -> u64 {
    let digest = sha256(format!("{discord_id}:{target_level}").as_bytes());
    u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 has eight bytes"))
}

/// Match Python's `random.Random(seed).sample(perks, min(4, len(perks)))`.
///
/// CPython uses the pool branch of `random.sample` for this at-most-12 item
/// catalog. Reproducing its integer seeding, MT19937, and `_randbelow` makes
/// an in-flight preview stable across both process restarts and runtimes.
#[must_use]
pub fn prestige_perk_choices(eligible: &[&str], discord_id: i64, target_level: i32) -> Vec<String> {
    let count = eligible.len().min(PRESTIGE_PICK_COUNT);
    let mut random = PythonIntegerRandom::new(prestige_picker_seed(discord_id, target_level));
    let mut pool = eligible.to_vec();
    let mut selected = Vec::with_capacity(count);
    for index in 0..count {
        let remaining = pool.len() - index;
        let choice = random.rand_below(remaining);
        selected.push(pool[choice].to_owned());
        pool[choice] = pool[remaining - 1];
    }
    selected
}

#[derive(Clone, Debug)]
struct PythonIntegerRandom {
    inner: PythonRandom,
}

impl PythonIntegerRandom {
    fn new(seed: u64) -> Self {
        Self {
            inner: PythonRandom::from_u64_seed(seed),
        }
    }

    fn from_words(words: &[u32]) -> Self {
        Self {
            inner: PythonRandom::from_u32_key(words),
        }
    }

    fn rand_below(&mut self, upper: usize) -> usize {
        assert!(upper > 0, "random upper bound must be positive");
        self.inner.randbelow(upper)
    }
}

fn sha256(input: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    let bit_length = (input.len() as u64) * 8;
    let mut message = input.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = INITIAL;
    for chunk in message.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, bytes) in chunk.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        for index in 16..64 {
            let sigma_zero = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let sigma_one = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(sigma_zero)
                .wrapping_add(words[index - 7])
                .wrapping_add(sigma_one);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let upper_e = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temporary_one = h
                .wrapping_add(upper_e)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let upper_a = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temporary_two = upper_a.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temporary_one);
            d = c;
            c = b;
            b = a;
            a = temporary_one.wrapping_add(temporary_two);
        }
        for (current, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *current = current.wrapping_add(value);
        }
    }

    let mut digest = [0_u8; 32];
    for (bytes, value) in digest.chunks_exact_mut(4).zip(state) {
        bytes.copy_from_slice(&value.to_be_bytes());
    }
    digest
}

#[cfg(test)]
#[path = "dig_prestige_runtime/tests.rs"]
mod tests;
