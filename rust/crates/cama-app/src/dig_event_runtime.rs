//! Complete existing-schema application orchestration for `/dig` event views.
//!
//! Discord supplies only actor/guild/event/choice/interaction identity. This
//! service snapshots every policy input, consumes deterministic retry-safe
//! entropy, applies the frozen canonical event policy, preserves Python's
//! modifier -> splash -> actor -> chain -> quest ordering, and returns the
//! entire typed result needed to render or recover the interaction.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cama_db::dig_event_runtime::{
    AtomicDigEventSettlement, DigEventActorKey, DigEventActorSnapshot, DigEventDeliveryDraft,
    DigEventDeliveryMark, DigEventDeliveryQuery,
    DigEventDeliverySnapshot as DbDigEventDeliverySnapshot,
    DigEventDeliveryState as DbDigEventDeliveryState, DigEventFinaleJcRequest,
    DigEventFinaleRelicRequest, DigEventGuildModifierReceipt, DigEventGuildModifierRequest,
    DigEventQuestMutation, DigEventQuestSnapshot, DigEventRewardQueryPlan,
    DigEventRuntimeRepository, DigEventRuntimeRepositoryError, DigEventSettlementOutcome,
    DigHostileSplashAuditRequest, DigSplashGrantRequest, EventJsonMutation,
};
use cama_db::dig_tunnel_encounters_repository::{
    DigTunnelEncounterRepository, EncounterCandidateQuery, EncounterVictimStrategy,
};
use cama_db::loan_repository::{LedgerContext, LoanRepository};
use cama_db::mana_service_repository::ManaRepository;
use cama_db::shop_runtime::{HostileDestination, HostileLossRequest, ShopRuntimeRepository};
use cama_domain::dig_economy::scale_positive_dig_jc;
use cama_domain::dig_splash::{HOSTILE_LOSS_MIN_BALANCE, strengthen_dig_event_penalty};
use cama_domain::economy_scaling::{scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta};
use cama_domain::game_date::game_date_for_timestamp;
use cama_domain::mana::ManaEffects;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::dig_loot::{
    CANONICAL_EVENT_CHAIN_CHANCE, CanonicalEventPolicy, CanonicalEventPresentation,
    CanonicalEventResolution, CanonicalEventResolutionRequest, CanonicalEventRollContext,
    CanonicalEventRolls, CanonicalQuestFinale, CanonicalQuestFinaleOutcome, CanonicalQuestProgress,
    CanonicalReward, CanonicalSplash, EventComplexity, LootEntropy, SeededLootEntropy,
    advance_canonical_quest_on_desperate_success, artifact_catalog, canonical_chain_event,
    canonical_eligible_events, canonical_eligible_quest_event_ids, canonical_event,
    canonical_event_needs_cruel_echo_roll, canonical_event_presentation, canonical_quest_for_event,
    resolve_canonical_event_with_policy, resolve_canonical_event_with_policy_and_rewards,
    resolve_canonical_quest_finale, roll_canonical_event, scale_canonical_splash_payout,
    select_canonical_reward,
};
use crate::dig_runtime::{DigRuntimeConfig, DigRuntimeEventRequest};
use crate::dig_tunnels::ascension_effects;
use crate::economy_event_sqlite::SqliteEconomyEventService;

const EVENT_INVENTORY_CAPACITY: usize = 8;
pub const DIG_EVENT_VIEW_TIMEOUT_SECONDS: i64 = 60;
const RANDOM_ACTIVE_LOOKBACK_SECONDS: i64 = 14 * 24 * 60 * 60;
const ACTIVE_DIGGER_LOOKBACK_SECONDS: i64 = 7 * 24 * 60 * 60;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigEventSplashOutcome {
    pub strategy: String,
    pub event_name: String,
    pub victims: Vec<(i64, i64)>,
    pub total_moved: i64,
    pub mode: String,
    pub absorbed_total: i64,
    pub shielded_count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigEventGuildModifierOutcome {
    pub modifier_id: String,
    pub duration_seconds: i64,
    pub payload: Value,
    pub expires_at: i64,
    pub applied_now: bool,
}

/// All request-local inputs for one canonical event roll.  Keeping the
/// policy inputs typed prevents the live Dig callsite from silently dropping
/// post-boss depth, Void Bait, or ascension rarity modifiers.
pub struct CanonicalEventRollInput<'a> {
    pub snapshot: &'a DigEventActorSnapshot,
    pub quest_snapshot: &'a DigEventQuestSnapshot,
    pub include_quest_events: bool,
    pub in_boss: bool,
    pub void_bait_active: bool,
    pub rare_event_multiplier: f64,
    pub legendary_event_multiplier: f64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DigEventQuestFinale {
    JcAndModifier {
        quest_id: String,
        gross_jc: i64,
        net_jc: i64,
        balance_after: Option<i64>,
        modifier: Option<DigEventGuildModifierOutcome>,
    },
    Relic {
        quest_id: String,
        relic_name: String,
        artifact_id: String,
        relic_stat_ids: Vec<String>,
        reward_row_id: Option<i64>,
    },
}

/// Full event payload retained through Discord presentation and restart.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DigEventRuntimeOutcome {
    pub success: bool,
    pub error: Option<String>,
    pub resolution: Option<CanonicalEventResolution>,
    pub depth_before: i64,
    pub depth_after: i64,
    pub balance_after: i64,
    pub action_id: Option<i64>,
    pub reward_row_id: Option<i64>,
    pub applied_now: bool,
    pub splash: Option<DigEventSplashOutcome>,
    pub guild_modifier: Option<DigEventGuildModifierOutcome>,
    pub chain_event: Option<CanonicalEventPresentation>,
    pub quest_finale: Option<DigEventQuestFinale>,
}

impl DigEventRuntimeOutcome {
    fn blocked(message: impl Into<String>) -> Self {
        Self {
            success: false,
            error: Some(message.into()),
            resolution: None,
            depth_before: 0,
            depth_after: 0,
            balance_after: 0,
            action_id: None,
            reward_row_id: None,
            applied_now: false,
            splash: None,
            guild_modifier: None,
            chain_event: None,
            quest_finale: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum DigEventRuntimeError {
    #[error("Dig event policy failed: {0}")]
    Policy(String),
    #[error("Dig event persistence failed: {0}")]
    EventRepository(#[from] DigEventRuntimeRepositoryError),
    #[error("Dig event splash failed: {0}")]
    Splash(String),
    #[error("Dig event economy policy failed: {0}")]
    Economy(String),
    #[error("Dig event quest finale failed: {0}")]
    Finale(String),
}

#[derive(Clone, Debug)]
pub struct DigEventRuntimeService {
    path: PathBuf,
    config: DigRuntimeConfig,
    finale_tax_hook: Option<fn(i64) -> i64>,
}

/// A small typed seam for the legacy quest service's `tax_fn` callback.
///
/// The callback receives the already-scaled personal finale payout. Keeping
/// it as a function pointer makes the application policy deterministic and
/// restart-safe while allowing the runtime/provider layer to bind its own
/// economy tax policy without importing Python callbacks.
pub type DigEventFinaleTaxHook = fn(i64) -> i64;

/// Public event-view projection for a unique gear reward. The Discord
/// provider can render this as the `Gear Drop` field without reconstructing
/// event-specific copy or trusting an untyped JSON payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigEventGearDropPresentation {
    pub field_name: String,
    pub field_value: String,
}

#[must_use]
pub fn gear_drop_presentation(
    name: &str,
    slot: &str,
    durability: i64,
    effect: &str,
) -> DigEventGearDropPresentation {
    let slot = slot
        .chars()
        .next()
        .map(|first| {
            first.to_uppercase().collect::<String>()
                + slot.get(first.len_utf8()..).unwrap_or_default()
        })
        .unwrap_or_default();
    DigEventGearDropPresentation {
        field_name: "Gear Drop".to_owned(),
        field_value: format!(
            "**{name}**\n{slot}\nDurability: {durability}\n{effect}\nStored in your gear inventory. View with `/dig gear`."
        ),
    }
}

/// Compact Discord component request for an event emitted by a committed Dig.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigEventActionRequest<'a> {
    pub discord_id: i64,
    pub guild_id: i64,
    pub dig_action_id: i64,
    pub choice: &'a str,
    pub now: i64,
}

/// Discord identity captured before an event result is published.  Event
/// results are a separate public message from the source event controls, so
/// READY recovery must not depend on the original interaction object.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigEventDeliveryContext {
    pub discord_id: i64,
    pub guild_id: i64,
    pub interaction_id: u64,
    pub channel_id: i64,
}

impl DigEventDeliveryContext {
    #[must_use]
    pub const fn new(discord_id: i64, guild_id: i64, interaction_id: u64, channel_id: i64) -> Self {
        Self {
            discord_id,
            guild_id,
            interaction_id,
            channel_id,
        }
    }
}

/// Immutable typed result retained beside the actor event settlement.  The
/// payload is the exact `DigEventRuntimeOutcome` returned to the provider;
/// transport code can render it without replaying policy or querying newer
/// tunnel state after a crash.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DigEventDeliverySnapshot {
    pub action_id: i64,
    pub source_key: String,
    pub discord_id: i64,
    pub guild_id: i64,
    pub context: DigEventDeliveryContext,
    pub committed_at: i64,
    pub state: DigEventDeliveryState,
    pub outcome: DigEventRuntimeOutcome,
    pub delivered_at: Option<i64>,
}

/// Durable delivery lifecycle. `Pending` is an actor-committed result whose
/// application follow-up still needs recovery; only `Ready` may be handed to
/// a transport, and `Delivered` is immutable after its CAS.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DigEventDeliveryState {
    Pending,
    Ready,
    Delivered,
}

impl DigEventDeliverySnapshot {
    #[must_use]
    pub fn nonce(&self) -> String {
        // Discord snowflakes fit in signed 64-bit today, but formatting the
        // original u64 avoids a lossy cast and keeps this stable across retry.
        format!("cama-e:{:x}", self.context.interaction_id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigEventPendingDeliveryQuery {
    pub guild_id: Option<i64>,
    pub discord_id: Option<i64>,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigEventDeliveryMarkRequest {
    pub action_id: i64,
    pub discord_id: i64,
    pub guild_id: i64,
    pub source_key: String,
    pub delivered_at: i64,
}

/// A Python legacy integer field. `EVENT_POOL` historically accepted either a
/// scalar or an inclusive two-element range for `jc` and `depth`/`advance`.
/// The parser keeps that distinction so scalar fields do not consume entropy,
/// while ranges use the same inclusive sampling semantics as Python's
/// `random.randint`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigLegacyIntegerRange {
    pub minimum: i64,
    pub maximum: i64,
}

impl DigLegacyIntegerRange {
    #[must_use]
    pub const fn scalar(value: i64) -> Self {
        Self {
            minimum: value,
            maximum: value,
        }
    }

    #[must_use]
    pub const fn is_scalar(self) -> bool {
        self.minimum == self.maximum
    }

    fn sample(self, entropy: &mut impl LootEntropy) -> i64 {
        if self.is_scalar() {
            self.minimum
        } else {
            entropy.advance(self.minimum, self.maximum)
        }
    }

    fn parse(value: Option<&Value>, field: &str) -> Result<Self, String> {
        let Some(value) = value else {
            return Ok(Self::scalar(0));
        };
        if let Some(number) = value.as_i64() {
            return Ok(Self::scalar(number));
        }
        let Some(values) = value.as_array() else {
            return Err(format!(
                "legacy event {field} must be an integer or [min, max]"
            ));
        };
        if values.len() != 2 {
            return Err(format!(
                "legacy event {field} range must contain two integers"
            ));
        }
        let minimum = values[0]
            .as_i64()
            .ok_or_else(|| format!("legacy event {field} range minimum must be an integer"))?;
        let maximum = values[1]
            .as_i64()
            .ok_or_else(|| format!("legacy event {field} range maximum must be an integer"))?;
        if minimum > maximum {
            return Err(format!(
                "legacy event {field} range minimum cannot exceed maximum"
            ));
        }
        Ok(Self { minimum, maximum })
    }
}

/// One typed outcome from a pre-catalog `EVENT_POOL` event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigLegacyEventOutcome {
    pub message: String,
    pub jc: DigLegacyIntegerRange,
    pub advance: DigLegacyIntegerRange,
}

/// One typed legacy `EVENT_POOL` event. These rows are intentionally separate
/// from [`CanonicalEventDef`]: the generated catalog contains no legacy
/// `outcomes` rows, but a migration/import boundary can still resolve one
/// without reintroducing an untyped JSON path into actor settlement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigLegacyEventDef {
    pub id: String,
    pub name: String,
    pub outcomes: BTreeMap<String, DigLegacyEventOutcome>,
}

/// Owned, validated adapter for a Python `EVENT_POOL` JSON snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigLegacyEventCatalog {
    events: BTreeMap<String, DigLegacyEventDef>,
}

impl DigLegacyEventCatalog {
    /// Parse an `EVENT_POOL` array, or a wrapper object containing `events`,
    /// `event_pool`, or `EVENT_POOL`. A wrapper is useful when the snapshot is
    /// transported alongside catalog metadata.
    pub fn from_json_str(raw: &str) -> Result<Self, String> {
        let value = serde_json::from_str::<Value>(raw)
            .map_err(|error| format!("invalid legacy event catalog JSON: {error}"))?;
        Self::from_json(&value)
    }

    pub fn from_json(value: &Value) -> Result<Self, String> {
        let rows = match value {
            Value::Array(rows) => rows,
            Value::Object(object) => ["events", "event_pool", "EVENT_POOL"]
                .iter()
                .find_map(|key| object.get(*key).and_then(Value::as_array))
                .ok_or_else(|| {
                    "legacy event catalog must be an EVENT_POOL array or wrapper".to_owned()
                })?,
            _ => return Err("legacy event catalog must be an array".to_owned()),
        };
        let mut events = BTreeMap::new();
        for row in rows {
            let event = parse_legacy_event(row)?;
            if events.insert(event.id.clone(), event).is_some() {
                return Err("legacy event catalog contains duplicate event ids".to_owned());
            }
        }
        Ok(Self { events })
    }

    #[must_use]
    pub fn event(&self, event_id: &str) -> Option<&DigLegacyEventDef> {
        self.events.get(event_id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

fn parse_legacy_event(value: &Value) -> Result<DigLegacyEventDef, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "legacy EVENT_POOL entries must be objects".to_owned())?;
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "legacy event id must be a non-empty string".to_owned())?
        .to_owned();
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Unknown Event")
        .to_owned();
    let outcomes = object
        .get("outcomes")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("legacy event {id} must contain an outcomes object"))?;
    let mut typed_outcomes = BTreeMap::new();
    for (choice, payload) in outcomes {
        if choice.trim().is_empty() {
            return Err(format!("legacy event {id} contains an empty choice"));
        }
        let payload = payload
            .as_object()
            .ok_or_else(|| format!("legacy event {id} outcome {choice} must be an object"))?;
        let message = payload
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Nothing happened.")
            .to_owned();
        // Python's legacy resolver called this field `depth`; a few imported
        // snapshots used the newer application spelling `advance`. Prefer the
        // explicit advance key when both are present, while accepting either.
        let advance = DigLegacyIntegerRange::parse(
            payload.get("advance").or_else(|| payload.get("depth")),
            "advance",
        )?;
        let jc = DigLegacyIntegerRange::parse(payload.get("jc"), "jc")?;
        if typed_outcomes
            .insert(
                choice.clone(),
                DigLegacyEventOutcome {
                    message,
                    jc,
                    advance,
                },
            )
            .is_some()
        {
            return Err(format!(
                "legacy event {id} contains duplicate choice {choice}"
            ));
        }
    }
    Ok(DigLegacyEventDef {
        id,
        name,
        outcomes: typed_outcomes,
    })
}

/// Request for the migration-only catalog dispatch path. The catalog owns the
/// event name, message, and scalar/range fields; the caller supplies only the
/// actor, choice, and durable interaction identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigLegacyCatalogRequest<'a> {
    pub discord_id: i64,
    pub guild_id: i64,
    pub event_id: &'a str,
    pub choice: &'a str,
    pub event_key: &'a str,
    pub now: i64,
}

/// Authored payload for a pre-catalog event row.  Python historically allowed
/// `EVENT_POOL` entries with an `outcomes` map instead of the newer typed
/// `safe_option`/`risky_option` shape.  This scalar request remains available
/// to callers that have already performed the legacy parse; new boundaries
/// should prefer [`DigLegacyEventCatalog`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigLegacyEventRequest<'a> {
    pub discord_id: i64,
    pub guild_id: i64,
    pub event_id: &'a str,
    pub event_name: &'a str,
    pub choice: &'a str,
    pub message: &'a str,
    pub advance: i64,
    pub jc: i64,
    pub event_key: &'a str,
    pub now: i64,
}

/// Durable event-prompt policy loaded from the same actor/action identity used
/// by settlement. Discord never has to infer pitch-black controls or inspect
/// prestige JSON itself, and retries select the same atmospheric hint.
#[derive(Clone, Debug, PartialEq)]
pub struct DigEventActionPresentation {
    pub event: CanonicalEventPresentation,
    pub luminosity: i64,
    pub safe_disabled: bool,
    pub reading_the_stone_hint: Option<String>,
}

const READING_THE_STONE_SAFE_HINTS: [&str; 3] = [
    "The walls whisper of patience here.",
    "A familiar rhythm — caution holds today.",
    "Stillness gathers along the safer passage.",
];
const READING_THE_STONE_RISKY_HINTS: [&str; 3] = [
    "The stones hum louder beside the bolder path.",
    "Something glints just past the edge of the dark.",
    "An unseen pull tugs you onward.",
];
const READING_THE_STONE_DESPERATE_HINTS: [&str; 3] = [
    "Old bones remember reckless feet.",
    "The rock itself seems to dare you forward.",
    "A wild current beckons from the deepest dark.",
];

impl DigEventRuntimeService {
    #[must_use]
    pub fn sqlite(path: impl AsRef<Path>) -> Self {
        Self::sqlite_with_config(path, DigRuntimeConfig::default())
    }

    #[must_use]
    pub fn sqlite_with_config(path: impl AsRef<Path>, config: DigRuntimeConfig) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            config,
            finale_tax_hook: None,
        }
    }

    /// Compose the event runtime from the same live Dig policy object used by
    /// the normal Dig service, including the optional legacy finale tax hook.
    /// Keeping this constructor explicit prevents provider callsites from
    /// silently falling back to `DigRuntimeConfig::default()`.
    #[must_use]
    pub fn sqlite_with_config_and_finale_tax_hook(
        path: impl AsRef<Path>,
        config: DigRuntimeConfig,
        finale_tax_hook: Option<DigEventFinaleTaxHook>,
    ) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            config,
            finale_tax_hook,
        }
    }

    /// Bind the legacy quest finale tax callback at the typed application
    /// boundary. The hook is intentionally optional so the default runtime
    /// remains identical to the no-tax path.
    #[must_use]
    pub fn with_finale_tax_hook(mut self, hook: DigEventFinaleTaxHook) -> Self {
        self.finale_tax_hook = Some(hook);
        self
    }

    #[must_use]
    pub const fn config(&self) -> &DigRuntimeConfig {
        &self.config
    }

    /// Roll an event from one already-loaded actor/tunnel snapshot.
    ///
    /// This is the application seam used by callers that have a tunnel in
    /// scope already (for example, `/dig` preconditions). It deliberately
    /// accepts the snapshot instead of fetching it again, and resolves quest
    /// eligibility from the separately loaded quest snapshot. Thus a picker
    /// can pass the tunnel through to the quest filter without a second DB
    /// read, matching Python's `roll_event(..., tunnel=...)` contract.
    pub fn roll_event_for_snapshot(
        &self,
        snapshot: &DigEventActorSnapshot,
        quest_snapshot: &DigEventQuestSnapshot,
        include_quest_events: bool,
        in_boss: bool,
        entropy: &mut impl LootEntropy,
    ) -> Option<CanonicalEventPresentation> {
        let void_bait_active = snapshot
            .temp_buff_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .and_then(|value| {
                value
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| id == "void_bait")
            })
            .unwrap_or(false);
        self.roll_event_for_snapshot_with_modifiers(
            CanonicalEventRollInput {
                snapshot,
                quest_snapshot,
                include_quest_events,
                in_boss,
                void_bait_active,
                rare_event_multiplier: 0.0,
                legendary_event_multiplier: 0.0,
            },
            entropy,
        )
    }

    /// Roll the canonical catalog against the post-gate runtime context.
    /// Unlike the compatibility wrapper above, this accepts the live
    /// `void_bait_digs` charge and ascension rarity multipliers explicitly;
    /// the tunnel snapshot remains the source for all other quest/predicate
    /// context.
    pub fn roll_event_for_snapshot_with_modifiers(
        &self,
        input: CanonicalEventRollInput<'_>,
        entropy: &mut impl LootEntropy,
    ) -> Option<CanonicalEventPresentation> {
        let predicates = if input.quest_snapshot.recent_bet {
            BTreeSet::from(["bet_within_7d".to_owned()])
        } else {
            BTreeSet::new()
        };
        let eligible_quest_ids = canonical_eligible_quest_event_ids(
            input.snapshot.depth,
            input.snapshot.prestige_level,
            input.quest_snapshot.active_quest_id.as_deref(),
            input.quest_snapshot.active_quest_step,
            &input.quest_snapshot.completed_quest_ids,
            &predicates,
        );
        let context = CanonicalEventRollContext {
            depth: input.snapshot.depth,
            luminosity: input.snapshot.luminosity,
            prestige_level: input.snapshot.prestige_level,
            eligible_quest_ids: &eligible_quest_ids,
            include_quest_events: input.include_quest_events,
            in_boss: input.in_boss,
            void_bait_active: input.void_bait_active,
            rare_event_multiplier: input.rare_event_multiplier,
            legendary_event_multiplier: input.legendary_event_multiplier,
        };
        roll_canonical_event(context, entropy)
            .and_then(|event| canonical_event_presentation(&event.id).ok())
    }

    /// Return the event-picker projection for an already-loaded actor and
    /// quest snapshot. This is the non-random companion to
    /// [`Self::roll_event_for_snapshot`] and makes the no-second-fetch
    /// contract directly testable.
    pub fn eligible_event_presentations_for_snapshot(
        &self,
        snapshot: &DigEventActorSnapshot,
        quest_snapshot: &DigEventQuestSnapshot,
        include_quest_events: bool,
        in_boss: bool,
    ) -> Vec<CanonicalEventPresentation> {
        let predicates = if quest_snapshot.recent_bet {
            BTreeSet::from(["bet_within_7d".to_owned()])
        } else {
            BTreeSet::new()
        };
        let eligible_quest_ids = canonical_eligible_quest_event_ids(
            snapshot.depth,
            snapshot.prestige_level,
            quest_snapshot.active_quest_id.as_deref(),
            quest_snapshot.active_quest_step,
            &quest_snapshot.completed_quest_ids,
            &predicates,
        );
        canonical_eligible_events(
            snapshot.depth,
            snapshot.luminosity,
            snapshot.prestige_level,
            &eligible_quest_ids,
            include_quest_events,
            in_boss,
        )
        .iter()
        .filter_map(|event| canonical_event_presentation(&event.id).ok())
        .collect()
    }

    pub fn presentation(
        &self,
        event_id: &str,
    ) -> Result<CanonicalEventPresentation, DigEventRuntimeError> {
        canonical_event_presentation(event_id).map_err(DigEventRuntimeError::Policy)
    }

    /// Load the authored presentation using the durable Dig action identity.
    pub fn presentation_for_action(
        &self,
        discord_id: i64,
        guild_id: i64,
        dig_action_id: i64,
    ) -> Result<Option<CanonicalEventPresentation>, DigEventRuntimeError> {
        let repository = DigEventRuntimeRepository::new(&self.path);
        repository
            .pending_event(dig_action_id, discord_id, Some(guild_id))?
            .map(|pending| self.presentation(&pending.event_id))
            .transpose()
    }

    /// Load the exact live prompt policy for a committed Dig action. The
    /// action lookup enforces actor/guild ownership, while the actor snapshot
    /// supplies the luminosity and perk state used by Python's view builder.
    pub fn action_presentation(
        &self,
        discord_id: i64,
        guild_id: i64,
        dig_action_id: i64,
        now: i64,
    ) -> Result<Option<DigEventActionPresentation>, DigEventRuntimeError> {
        let repository = DigEventRuntimeRepository::new(&self.path);
        let Some(pending) = repository.pending_event(dig_action_id, discord_id, Some(guild_id))?
        else {
            return Ok(None);
        };
        if event_view_expired(pending.created_at, now) {
            return Ok(None);
        }
        let Some(snapshot) = repository.actor_snapshot_for_event(DigEventActorKey {
            discord_id,
            guild_id: Some(guild_id),
        })?
        else {
            return Ok(None);
        };
        canonical_event_action_presentation(
            &pending.event_id,
            snapshot.luminosity,
            &snapshot.prestige_perks_json,
            dig_action_id,
        )
        .map(Some)
    }

    /// Resolve a Discord event component without trusting an event id carried
    /// by the client. `dig_action_id` is also the stable idempotency key, so
    /// retries and double clicks return the original settlement receipt.
    pub fn resolve_action_event(
        &self,
        request: DigEventActionRequest<'_>,
    ) -> Result<DigEventRuntimeOutcome, DigEventRuntimeError> {
        let repository = DigEventRuntimeRepository::new(&self.path);
        let Some(pending) = repository.pending_event(
            request.dig_action_id,
            request.discord_id,
            Some(request.guild_id),
        )?
        else {
            return Ok(DigEventRuntimeOutcome::blocked(
                "This Dig event expired or belongs to another player.",
            ));
        };
        if event_view_expired(pending.created_at, request.now) {
            return Ok(DigEventRuntimeOutcome::blocked(
                "This Dig event expired or belongs to another player.",
            ));
        }
        let event_key = format!("dig-action:{}", pending.action_id);
        self.resolve_event(DigRuntimeEventRequest {
            discord_id: request.discord_id,
            guild_id: request.guild_id,
            event_id: &pending.event_id,
            choice: request.choice,
            event_key: &event_key,
            now: request.now,
            chained: false,
        })
    }

    /// Resolve an event and atomically seed its immutable public-result
    /// outbox. The event settlement itself receives the delivery draft when a
    /// caller supplies this context; the separate persistence helper remains
    /// useful for upgrading already-settled pre-outbox actions.
    pub fn resolve_action_event_with_delivery(
        &self,
        request: DigEventActionRequest<'_>,
        context: DigEventDeliveryContext,
    ) -> Result<DigEventRuntimeOutcome, DigEventRuntimeError> {
        let repository = DigEventRuntimeRepository::new(&self.path);
        let Some(pending) = repository.pending_event(
            request.dig_action_id,
            request.discord_id,
            Some(request.guild_id),
        )?
        else {
            return Ok(DigEventRuntimeOutcome::blocked(
                "This Dig event expired or belongs to another player.",
            ));
        };
        if event_view_expired(pending.created_at, request.now) {
            return Ok(DigEventRuntimeOutcome::blocked(
                "This Dig event expired or belongs to another player.",
            ));
        }
        let event_key = format!("dig-action:{}", pending.action_id);
        self.resolve_event_with_delivery(
            DigRuntimeEventRequest {
                discord_id: request.discord_id,
                guild_id: request.guild_id,
                event_id: &pending.event_id,
                choice: request.choice,
                event_key: &event_key,
                now: request.now,
                chained: false,
            },
            context,
        )
    }

    /// Persist one immutable event-result projection beside its resolved
    /// action. New provider paths should use
    /// [`Self::resolve_action_event_with_delivery`], which owns this call
    /// immediately after settlement; this method is intentionally public for
    /// READY migration of actions created before the outbox was introduced.
    pub fn persist_event_delivery(
        &self,
        outcome: &DigEventRuntimeOutcome,
        context: DigEventDeliveryContext,
        committed_at: i64,
    ) -> Result<Option<DigEventDeliverySnapshot>, DigEventRuntimeError> {
        let Some(action_id) = outcome.action_id else {
            return Ok(None);
        };
        if !outcome.success {
            return Ok(None);
        }
        let outcome_json = serde_json::to_string(outcome)
            .map_err(|error| DigEventRuntimeError::Policy(error.to_string()))?;
        let source_key = format!("dig-event:{action_id}");
        let db_snapshot = DigEventRuntimeRepository::new(&self.path).attach_event_delivery(
            action_id,
            DigEventDeliveryDraft {
                source_key: &source_key,
                discord_id: context.discord_id,
                guild_id: context.guild_id,
                interaction_id: context.interaction_id,
                channel_id: context.channel_id,
                committed_at,
                outcome_json: &outcome_json,
            },
        )?;
        Ok(Some(event_delivery_from_db(db_snapshot)?))
    }

    pub fn pending_event_deliveries(
        &self,
        query: DigEventPendingDeliveryQuery,
    ) -> Result<Vec<DigEventDeliverySnapshot>, DigEventRuntimeError> {
        DigEventRuntimeRepository::new(&self.path)
            .pending_event_deliveries(DigEventDeliveryQuery {
                guild_id: query.guild_id,
                discord_id: query.discord_id,
                limit: query.limit,
            })?
            .into_iter()
            .map(event_delivery_from_db)
            .collect()
    }

    /// List actor-committed event results that stopped between the SQLite
    /// settlement and the application-owned quest/finale follow-up. These
    /// rows are deliberately not returned by [`Self::pending_event_deliveries`]
    /// until [`Self::recover_event_delivery`] freezes their complete outcome.
    pub fn pending_event_delivery_recoveries(
        &self,
        query: DigEventPendingDeliveryQuery,
    ) -> Result<Vec<DigEventDeliverySnapshot>, DigEventRuntimeError> {
        DigEventRuntimeRepository::new(&self.path)
            .pending_event_delivery_recoveries(DigEventDeliveryQuery {
                guild_id: query.guild_id,
                discord_id: query.discord_id,
                limit: query.limit,
            })?
            .into_iter()
            .map(event_delivery_from_db)
            .collect()
    }

    /// Replay one pending event's idempotent application follow-up and freeze
    /// the resulting typed payload as `ready`. The actor mutation is guarded
    /// by the original event key, so this does not mint a second actor action;
    /// quest/finale persistence and delivery CAS remain retry-safe.
    pub fn recover_event_delivery(
        &self,
        action_id: i64,
    ) -> Result<Option<DigEventRuntimeOutcome>, DigEventRuntimeError> {
        let repository = DigEventRuntimeRepository::new(&self.path);
        let recoveries = repository.pending_event_delivery_recoveries(DigEventDeliveryQuery {
            guild_id: None,
            discord_id: None,
            limit: usize::MAX,
        })?;
        let Some(delivery) = recoveries
            .into_iter()
            .find(|delivery| delivery.action_id == action_id)
        else {
            return Ok(None);
        };
        let Some(identity) = repository.resolved_event_identity(
            action_id,
            delivery.discord_id,
            delivery.guild_id,
        )?
        else {
            return Ok(None);
        };
        let context = DigEventDeliveryContext::new(
            delivery.discord_id,
            delivery.guild_id,
            delivery.interaction_id,
            delivery.channel_id,
        );
        let outcome = self.resolve_event_with_delivery(
            DigRuntimeEventRequest {
                discord_id: identity.actor_id,
                guild_id: identity.guild_id,
                event_id: &identity.event_id,
                choice: &identity.choice,
                event_key: &identity.event_key,
                // Preserve the original policy date and economy window. A
                // recovery after midnight must not roll a different outcome.
                now: delivery.committed_at,
                chained: false,
            },
            context,
        )?;
        Ok(outcome.success.then_some(outcome))
    }

    pub fn mark_event_delivery_delivered(
        &self,
        request: DigEventDeliveryMarkRequest,
    ) -> Result<bool, DigEventRuntimeError> {
        Ok(
            DigEventRuntimeRepository::new(&self.path).mark_event_delivery_delivered(
                DigEventDeliveryMark {
                    action_id: request.action_id,
                    discord_id: request.discord_id,
                    guild_id: request.guild_id,
                    source_key: request.source_key,
                    delivered_at: request.delivered_at,
                },
            )?,
        )
    }

    /// Resolve a pending event through an explicitly supplied legacy catalog.
    /// This is the production migration/import boundary for an old
    /// `EVENT_POOL` snapshot. Canonical generated events continue through
    /// [`Self::resolve_action_event`]; only an event id absent from that
    /// catalog is admitted to the legacy parser.
    pub fn resolve_action_event_with_legacy_catalog(
        &self,
        request: DigEventActionRequest<'_>,
        catalog: &DigLegacyEventCatalog,
    ) -> Result<DigEventRuntimeOutcome, DigEventRuntimeError> {
        let repository = DigEventRuntimeRepository::new(&self.path);
        let Some(pending) = repository.pending_event(
            request.dig_action_id,
            request.discord_id,
            Some(request.guild_id),
        )?
        else {
            return Ok(DigEventRuntimeOutcome::blocked(
                "This Dig event expired or belongs to another player.",
            ));
        };
        if event_view_expired(pending.created_at, request.now) {
            return Ok(DigEventRuntimeOutcome::blocked(
                "This Dig event expired or belongs to another player.",
            ));
        }
        if canonical_event(&pending.event_id).is_some() {
            let event_key = format!("dig-action:{}", pending.action_id);
            return self.resolve_event(DigRuntimeEventRequest {
                discord_id: request.discord_id,
                guild_id: request.guild_id,
                event_id: &pending.event_id,
                choice: request.choice,
                event_key: &event_key,
                now: request.now,
                chained: false,
            });
        }
        let event_key = format!("dig-action:{}", pending.action_id);
        self.resolve_legacy_event_from_catalog(
            catalog,
            DigLegacyCatalogRequest {
                discord_id: request.discord_id,
                guild_id: request.guild_id,
                event_id: &pending.event_id,
                choice: request.choice,
                event_key: &event_key,
                now: request.now,
            },
        )
    }

    pub fn resolve_event(
        &self,
        request: DigRuntimeEventRequest<'_>,
    ) -> Result<DigEventRuntimeOutcome, DigEventRuntimeError> {
        self.resolve_event_inner(request, None)
    }

    /// Resolve one event while atomically seeding the immutable public-result
    /// delivery projection. The normal `resolve_event` API remains transport
    /// independent; provider code should use this variant when the result is
    /// going to Discord.
    pub fn resolve_event_with_delivery(
        &self,
        request: DigRuntimeEventRequest<'_>,
        context: DigEventDeliveryContext,
    ) -> Result<DigEventRuntimeOutcome, DigEventRuntimeError> {
        self.resolve_event_inner(request, Some(context))
    }

    fn resolve_event_inner(
        &self,
        request: DigRuntimeEventRequest<'_>,
        delivery_context: Option<DigEventDeliveryContext>,
    ) -> Result<DigEventRuntimeOutcome, DigEventRuntimeError> {
        if canonical_event(request.event_id).is_none() {
            return Ok(DigEventRuntimeOutcome::blocked("Unknown event."));
        }
        if request.event_key.trim().is_empty() {
            return Ok(DigEventRuntimeOutcome::blocked(
                "This event interaction is missing its durable identity.",
            ));
        }
        let key = DigEventActorKey {
            discord_id: request.discord_id,
            guild_id: Some(request.guild_id),
        };
        let repository = DigEventRuntimeRepository::new(&self.path);
        let Some(snapshot) = repository.actor_snapshot_for_event(key)? else {
            return Ok(DigEventRuntimeOutcome::blocked("You don't have a tunnel."));
        };
        let quest_snapshot = repository.quest_snapshot(key, request.now)?;
        let mut entropy = SeededLootEntropy::new(event_seed(request));
        let policy = event_policy(
            &snapshot,
            request.chained,
            self.config.minigame_jc_delta_scale,
        );
        let mut rolls = CanonicalEventRolls::default();
        if canonical_event_needs_cruel_echo_roll(request.event_id, request.choice, policy) {
            rolls.cruel_echo_roll = Some(entropy.unit());
        }
        rolls.success_roll = entropy.unit();

        // Resolve once to discover the selected authored branch. Python only
        // consumes reward/jitter draws when that branch actually needs them.
        let preview =
            resolve_canonical_event_with_policy(request.event_id, request.choice, rolls, policy)
                .map_err(DigEventRuntimeError::Policy)?;
        let reward_query_plan = DigEventRewardQueryPlan {
            owned_gear: !preview.gear_reward_pool.is_empty(),
            inventory_count: !preview.consumable_reward_pool.is_empty(),
            owned_artifacts: !preview.artifact_reward_pool.is_empty(),
        };
        let reward_snapshot = repository.reward_snapshot(key, reward_query_plan)?;
        let reward_probe = select_canonical_reward(
            &resolution_as_outcome(&preview),
            &reward_snapshot.owned_gear,
            &reward_snapshot.owned_artifacts,
            reward_snapshot.inventory_count,
            EVENT_INVENTORY_CAPACITY,
            0.0,
        )
        .map_err(DigEventRuntimeError::Policy)?;
        if reward_probe.reward.is_some() {
            rolls.reward_roll = Some(entropy.unit());
        }
        if preview.random_plan.jc_jitter {
            rolls.jc_jitter_multiplier = 0.5 + entropy.unit();
        }
        if preview.random_plan.advance_jitter {
            rolls.advance_jitter = entropy.advance(-2, 2);
        }
        let mut resolution =
            resolve_canonical_event_with_policy_and_rewards(CanonicalEventResolutionRequest {
                event_id: request.event_id,
                requested_choice: request.choice,
                rolls,
                policy,
                owned_gear: &reward_snapshot.owned_gear,
                owned_artifacts: &reward_snapshot.owned_artifacts,
                inventory_len: reward_snapshot.inventory_count,
                inventory_capacity: EVENT_INVENTORY_CAPACITY,
            })
            .map_err(DigEventRuntimeError::Policy)?;

        // Python sets the guild-wide window before touching splash victims or
        // the actor transaction, and failure is intentionally fail-soft.
        let guild_modifier = resolution
            .guild_modifier_on_success
            .as_ref()
            .and_then(authored_modifier)
            .and_then(|modifier| {
                repository
                    .set_guild_modifier(DigEventGuildModifierRequest {
                        guild_id: Some(request.guild_id),
                        actor_id: request.discord_id,
                        modifier_id: &modifier.modifier_id,
                        duration_seconds: modifier.duration_seconds,
                        payload_json: &modifier.payload.to_string(),
                        event_key: &format!("{}:modifier", request.event_key),
                        now: request.now,
                    })
                    .ok()
                    .map(|receipt| modifier.with_receipt(receipt))
            });

        let splash_execution = resolution.splash.as_ref().map_or_else(
            || Ok(SplashExecution::default()),
            |splash| self.resolve_splash(request, &resolution, splash, &mut entropy),
        )?;
        if let Some(splash) = resolution.splash.as_ref()
            && splash.mode == "burn"
            && resolution.economy_gross_jc > 0
        {
            let scaled = scale_canonical_splash_payout(
                resolution.economy_gross_jc,
                splash,
                splash_execution
                    .outcome
                    .as_ref()
                    .map_or(0, |outcome| outcome.total_moved),
                self.config.minigame_jc_delta_scale,
            );
            resolution.economy_gross_jc = scaled.jc_after;
            resolution.splash_payout_ratio = Some(scaled.payout_ratio);
        }
        resolution.jc =
            self.final_event_jc(request.guild_id, resolution.economy_gross_jc, request.now);

        let mut expected = snapshot.clone();
        expected.inventory_count = reward_snapshot.inventory_count;
        expected.owned_gear = reward_snapshot.owned_gear;
        expected.owned_artifacts = reward_snapshot.owned_artifacts;
        expected.balance = expected
            .balance
            .checked_add(splash_execution.actor_transfer)
            .ok_or_else(|| DigEventRuntimeError::Policy("Event balance overflow.".to_owned()))?;
        let depth_after = expected.depth.saturating_add(resolution.advance).max(0);
        let streak_after = resolution.streak_days_after.unwrap_or(expected.streak_days);
        let buff_json = resolution.buff.as_ref().map(persisted_buff_json);
        let curse_json = event_curse_replacement(&expected, &resolution);
        let reward = prepared_reward(&resolution)?;
        let reward_source = format!("event:{}", request.event_id);
        let detail = event_detail(&resolution, splash_execution.outcome.as_ref());
        // Chain selection is part of the immutable event result, so consume
        // it before the actor transaction and include it in the same outbox
        // snapshot as the actor settlement.
        let chain_event = resolve_chain(
            &resolution,
            depth_after,
            expected.prestige_level,
            &mut entropy,
        )
        .map(|event| canonical_event_presentation(&event.id))
        .transpose()
        .map_err(DigEventRuntimeError::Policy)?;
        if let Some(chain) = &chain_event {
            resolution.chained_event_id = Some(chain.event_id.clone());
        }
        let delivery_source_key = format!("dig-event:{}", request.event_key);
        let delivery_outcome_json = delivery_context
            .as_ref()
            .map(|_| {
                serde_json::to_string(&DigEventRuntimeOutcome {
                    success: true,
                    error: None,
                    resolution: Some(resolution.clone()),
                    depth_before: expected.depth,
                    depth_after,
                    balance_after: expected.balance.saturating_add(resolution.jc),
                    action_id: None,
                    reward_row_id: None,
                    applied_now: true,
                    splash: splash_execution.outcome.clone(),
                    guild_modifier: guild_modifier.clone(),
                    chain_event: chain_event.clone(),
                    quest_finale: None,
                })
            })
            .transpose()
            .map_err(|error| DigEventRuntimeError::Policy(error.to_string()))?;
        let delivery_draft = delivery_context
            .as_ref()
            .zip(delivery_outcome_json.as_ref())
            .map(|(context, outcome_json)| DigEventDeliveryDraft {
                source_key: &delivery_source_key,
                discord_id: context.discord_id,
                guild_id: context.guild_id,
                interaction_id: context.interaction_id,
                channel_id: context.channel_id,
                committed_at: request.now,
                outcome_json,
            });
        let settlement = repository.settle_actor_atomic_for_event(AtomicDigEventSettlement {
            expected: &expected,
            event_key: request.event_key,
            event_id: request.event_id,
            choice: &resolution.choice,
            depth_after,
            streak_days_after: streak_after,
            buff_mutation: buff_json
                .as_deref()
                .map_or(EventJsonMutation::Preserve, EventJsonMutation::Replace),
            curse_mutation: match curse_json.as_deref() {
                Some("") => EventJsonMutation::Clear,
                Some(value) => EventJsonMutation::Replace(value),
                None => EventJsonMutation::Preserve,
            },
            balance_delta: resolution.jc,
            reward: reward.as_ref().map(|reward| reward.as_db(&reward_source)),
            inventory_capacity: EVENT_INVENTORY_CAPACITY,
            detail_json: &detail,
            created_at: request.now,
            delivery: delivery_draft,
        })?;
        let receipt = match settlement {
            DigEventSettlementOutcome::Applied(receipt) => receipt,
            DigEventSettlementOutcome::Conflict => {
                return Ok(DigEventRuntimeOutcome::blocked(
                    "Your tunnel changed while this event was resolving. Try the choice again.",
                ));
            }
            DigEventSettlementOutcome::MissingTunnel => {
                return Ok(DigEventRuntimeOutcome::blocked("You don't have a tunnel."));
            }
            DigEventSettlementOutcome::MissingPlayer => {
                return Ok(DigEventRuntimeOutcome::blocked(
                    "You need to register first. Use /player register.",
                ));
            }
        };

        let quest_finale = self.resolve_quest_followup(
            request,
            &repository,
            &quest_snapshot,
            &resolution,
            depth_after,
            expected.prestige_level,
            receipt.applied_now,
            &mut entropy,
        );

        let outcome = DigEventRuntimeOutcome {
            success: true,
            error: None,
            resolution: Some(resolution),
            depth_before: receipt.depth_before,
            depth_after: receipt.depth_after,
            balance_after: receipt.balance_after,
            action_id: Some(receipt.action_id),
            reward_row_id: receipt.reward_row_id,
            applied_now: receipt.applied_now,
            splash: splash_execution.outcome,
            guild_modifier,
            chain_event,
            quest_finale,
        };
        if let Some(context) = delivery_context.as_ref()
            && outcome.action_id.is_some()
        {
            let outcome_json = serde_json::to_string(&outcome)
                .map_err(|error| DigEventRuntimeError::Policy(error.to_string()))?;
            repository.update_event_delivery_outcome(
                outcome.action_id.expect("checked above"),
                context.discord_id,
                context.guild_id,
                &delivery_source_key,
                &outcome_json,
            )?;
        }
        Ok(outcome)
    }

    /// Settle one legacy `outcomes`-shape event supplied by the runtime
    /// catalog adapter.  The payload is intentionally typed at this boundary:
    /// the provider may parse old JSON, but it cannot bypass the same actor
    /// snapshot, economy-before-positive-scale ordering, CAS settlement, or
    /// durable event-key idempotency used by canonical events.
    pub fn resolve_legacy_event(
        &self,
        request: DigLegacyEventRequest<'_>,
    ) -> Result<DigEventRuntimeOutcome, DigEventRuntimeError> {
        if request.event_id.trim().is_empty()
            || request.event_name.trim().is_empty()
            || request.choice.trim().is_empty()
        {
            return Ok(DigEventRuntimeOutcome::blocked("Invalid legacy event."));
        }
        if request.event_key.trim().is_empty() {
            return Ok(DigEventRuntimeOutcome::blocked(
                "This event interaction is missing its durable identity.",
            ));
        }
        let key = DigEventActorKey {
            discord_id: request.discord_id,
            guild_id: Some(request.guild_id),
        };
        let repository = DigEventRuntimeRepository::new(&self.path);
        let Some(snapshot) = repository.actor_snapshot_for_event(key)? else {
            return Ok(DigEventRuntimeOutcome::blocked("You don't have a tunnel."));
        };
        self.resolve_legacy_event_values(request, repository, snapshot)
    }

    /// Resolve one event from an owned legacy catalog. Range draws are seeded
    /// from the durable event key and actor identity, so a retry samples the
    /// same authored result before the repository's idempotency receipt is
    /// consulted.
    pub fn resolve_legacy_event_from_catalog(
        &self,
        catalog: &DigLegacyEventCatalog,
        request: DigLegacyCatalogRequest<'_>,
    ) -> Result<DigEventRuntimeOutcome, DigEventRuntimeError> {
        let event = catalog
            .event(request.event_id)
            .ok_or_else(|| DigEventRuntimeError::Policy("Unknown legacy event.".to_owned()))?;
        let outcome = event.outcomes.get(request.choice).ok_or_else(|| {
            DigEventRuntimeError::Policy(format!("Invalid choice: {}", request.choice))
        })?;
        let mut entropy = SeededLootEntropy::new(legacy_event_seed(request));
        let jc = outcome.jc.sample(&mut entropy);
        let advance = outcome.advance.sample(&mut entropy);
        self.resolve_legacy_event(DigLegacyEventRequest {
            discord_id: request.discord_id,
            guild_id: request.guild_id,
            event_id: &event.id,
            event_name: &event.name,
            choice: request.choice,
            message: &outcome.message,
            advance,
            jc,
            event_key: request.event_key,
            now: request.now,
        })
    }

    /// Parse and resolve one legacy catalog snapshot in one call. Deployment
    /// code can use this while importing a historical JSON pool, without
    /// exposing an untyped `Value` to the actor settlement API.
    pub fn resolve_legacy_event_json(
        &self,
        catalog_json: &str,
        request: DigLegacyCatalogRequest<'_>,
    ) -> Result<DigEventRuntimeOutcome, DigEventRuntimeError> {
        let catalog = DigLegacyEventCatalog::from_json_str(catalog_json)
            .map_err(DigEventRuntimeError::Policy)?;
        self.resolve_legacy_event_from_catalog(&catalog, request)
    }

    fn resolve_legacy_event_values(
        &self,
        request: DigLegacyEventRequest<'_>,
        repository: DigEventRuntimeRepository,
        snapshot: DigEventActorSnapshot,
    ) -> Result<DigEventRuntimeOutcome, DigEventRuntimeError> {
        let gross_jc =
            scale_minigame_jc_delta(request.jc as f64, self.config.minigame_jc_delta_scale);
        let final_jc = self.final_legacy_event_jc(request.guild_id, gross_jc, request.now);
        let mut advance = request.advance;
        let mut boss_encounter = false;
        if advance > 0
            && let Some(boundary) = next_boss_boundary(&snapshot.boss_progress_json)
            && snapshot.depth.saturating_add(advance) >= boundary
        {
            advance = boundary
                .saturating_sub(1)
                .saturating_sub(snapshot.depth)
                .max(0);
            boss_encounter = true;
        }
        let resolution = CanonicalEventResolution {
            event_id: request.event_id.to_owned(),
            event_name: request.event_name.to_owned(),
            choice: request.choice.to_owned(),
            complexity: EventComplexity::Choice,
            descriptions: vec![request.message.to_owned()],
            steps: Vec::new(),
            boon_options: Vec::new(),
            ascii_art: None,
            social: false,
            succeeded: true,
            message: request.message.to_owned(),
            advance,
            jc: final_jc,
            cave_in: false,
            streak_loss: 0,
            streak_days_after: None,
            curse: None,
            persisted_curse: None,
            buff: None,
            black_wax_seal_spent: false,
            active_curse_remaining_after: None,
            curse_cleared: false,
            gear_reward_pool: Vec::new(),
            consumable_reward_pool: Vec::new(),
            artifact_reward_pool: Vec::new(),
            splash: None,
            guild_modifier_on_success: None,
            quest_id: None,
            quest_step: None,
            next_event_id: None,
            chained_event_id: None,
            reward: None,
            duplicate_gear_reward: false,
            splash_payout_ratio: None,
            economy_gross_jc: request.jc,
            cruel_echoes: false,
            boss_encounter,
            random_plan: Default::default(),
        };
        let detail = event_detail(&resolution, None);
        let expected = snapshot.clone();
        let depth_after = expected.depth.saturating_add(advance).max(0);
        let settlement = repository.settle_actor_atomic_for_event(AtomicDigEventSettlement {
            expected: &expected,
            event_key: request.event_key,
            event_id: request.event_id,
            choice: request.choice,
            depth_after,
            streak_days_after: expected.streak_days,
            buff_mutation: EventJsonMutation::Preserve,
            curse_mutation: EventJsonMutation::Preserve,
            balance_delta: resolution.jc,
            reward: None,
            inventory_capacity: EVENT_INVENTORY_CAPACITY,
            detail_json: &detail,
            created_at: request.now,
            delivery: None,
        })?;
        let receipt = match settlement {
            DigEventSettlementOutcome::Applied(receipt) => receipt,
            DigEventSettlementOutcome::Conflict => {
                return Ok(DigEventRuntimeOutcome::blocked(
                    "Your tunnel changed while this event was resolving. Try the choice again.",
                ));
            }
            DigEventSettlementOutcome::MissingTunnel => {
                return Ok(DigEventRuntimeOutcome::blocked("You don't have a tunnel."));
            }
            DigEventSettlementOutcome::MissingPlayer => {
                return Ok(DigEventRuntimeOutcome::blocked(
                    "You need to register first. Use /player register.",
                ));
            }
        };
        Ok(DigEventRuntimeOutcome {
            success: true,
            error: None,
            resolution: Some(resolution),
            depth_before: receipt.depth_before,
            depth_after: receipt.depth_after,
            balance_after: receipt.balance_after,
            action_id: Some(receipt.action_id),
            reward_row_id: receipt.reward_row_id,
            applied_now: receipt.applied_now,
            splash: None,
            guild_modifier: None,
            chain_event: None,
            quest_finale: None,
        })
    }

    fn final_legacy_event_jc(&self, guild_id: i64, gross_jc: i64, now: i64) -> i64 {
        if gross_jc <= 0 {
            // Python's legacy path structurally scales a loss but does not
            // apply the deflationary event multiplier used by newer typed
            // events.
            return gross_jc;
        }
        let economy = SqliteEconomyEventService::new(&self.path, self.config.economy_event.clone());
        let adjusted = economy
            .adjust_reward_at(guild_id, gross_jc, now)
            .unwrap_or(gross_jc);
        scale_positive_dig_jc(adjusted)
    }

    fn final_event_jc(&self, guild_id: i64, authored: i64, now: i64) -> i64 {
        if authored < 0 {
            return scale_deflationary_minigame_jc_delta(
                authored as f64,
                self.config.minigame_jc_delta_scale,
            );
        }
        if authored == 0 {
            return 0;
        }
        let structural =
            scale_minigame_jc_delta(authored as f64, self.config.minigame_jc_delta_scale);
        let economy = SqliteEconomyEventService::new(&self.path, self.config.economy_event.clone());
        let adjusted = economy
            .adjust_reward_at(guild_id, structural, now)
            .unwrap_or(structural);
        scale_positive_dig_jc(adjusted)
    }

    fn resolve_splash(
        &self,
        request: DigRuntimeEventRequest<'_>,
        resolution: &CanonicalEventResolution,
        splash: &CanonicalSplash,
        entropy: &mut impl LootEntropy,
    ) -> Result<SplashExecution, DigEventRuntimeError> {
        if splash.victim_count == 0 || splash.penalty_jc <= 0 {
            return Ok(SplashExecution::empty(splash, &resolution.event_name));
        }
        let Some(strategy) = victim_strategy(&splash.strategy) else {
            return Ok(SplashExecution::empty(splash, &resolution.event_name));
        };
        // A replay must target the victims the first execution picked. Live
        // selection would choose different ones -- the first burns change the
        // balance ranking that `richest_n` orders by -- and those fresh victims
        // have no exact-once key, so they would be debited a second time.
        let frozen = DigEventRuntimeRepository::new(&self.path)
            .committed_splash_victims(Some(request.guild_id), request.event_key)
            .map_err(|error| DigEventRuntimeError::Splash(error.to_string()))?;
        let victim_ids = if frozen.is_empty() {
            let repository = DigTunnelEncounterRepository::new(&self.path);
            let (active_since, limit) =
                candidate_window(strategy, splash.victim_count, request.now);
            let candidates = repository
                .candidate_ids(EncounterCandidateQuery {
                    guild_id: Some(request.guild_id),
                    digger_discord_id: request.discord_id,
                    strategy,
                    active_since,
                    limit,
                })
                .map_err(|error| DigEventRuntimeError::Splash(error.to_string()))?;
            select_victims(candidates, splash.victim_count, strategy, entropy)
        } else {
            frozen
        };
        if splash.mode == "grant" {
            return self.resolve_grant_splash(request, resolution, splash, victim_ids);
        }
        self.resolve_hostile_splash(request, resolution, splash, victim_ids)
    }

    fn resolve_grant_splash(
        &self,
        request: DigRuntimeEventRequest<'_>,
        resolution: &CanonicalEventResolution,
        splash: &CanonicalSplash,
        victim_ids: Vec<i64>,
    ) -> Result<SplashExecution, DigEventRuntimeError> {
        let gross = scale_minigame_jc_delta(
            splash.penalty_jc as f64,
            self.config.minigame_jc_delta_scale,
        );
        let economy = SqliteEconomyEventService::new(&self.path, self.config.economy_event.clone());
        let adjusted = economy
            .adjust_reward_at(request.guild_id, gross, request.now)
            .unwrap_or(gross);
        let amount = scale_positive_dig_jc(adjusted);
        if amount <= 0 {
            return Ok(SplashExecution::empty(splash, &resolution.event_name));
        }
        let repository = DigEventRuntimeRepository::new(&self.path);
        let mut victims = Vec::new();
        for victim_id in victim_ids {
            let event_key = format!("{}:splash:{victim_id}", request.event_key);
            let detail = splash_detail(resolution, splash, amount, gross);
            match repository.settle_splash_grant_atomic(DigSplashGrantRequest {
                guild_id: Some(request.guild_id),
                digger_id: request.discord_id,
                victim_id,
                event_name: &resolution.event_name,
                strategy: &splash.strategy,
                event_key: &event_key,
                amount,
                gross_jc: gross,
                detail_json: &detail,
                created_at: request.now,
            }) {
                Ok(_) => victims.push((victim_id, amount)),
                Err(DigEventRuntimeRepositoryError::MissingSplashVictim) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(SplashExecution::from_victims(
            splash,
            &resolution.event_name,
            victims,
            0,
            0,
        ))
    }

    fn resolve_hostile_splash(
        &self,
        request: DigRuntimeEventRequest<'_>,
        resolution: &CanonicalEventResolution,
        splash: &CanonicalSplash,
        victim_ids: Vec<i64>,
    ) -> Result<SplashExecution, DigEventRuntimeError> {
        // Steal transfers the authored amount to the digger; only burn mode
        // applies the deflationary event penalty multiplier.  Python keeps
        // these two hostile modes distinct (`steal` deliberately bypasses
        // `strengthen_dig_event_penalty`).
        let amount = if splash.mode == "steal" {
            scale_minigame_jc_delta(
                splash.penalty_jc as f64,
                self.config.minigame_jc_delta_scale,
            )
        } else {
            scale_deflationary_minigame_jc_delta(
                strengthen_dig_event_penalty(splash.penalty_jc) as f64,
                self.config.minigame_jc_delta_scale,
            )
        };
        if amount <= 0 {
            return Ok(SplashExecution::empty(splash, &resolution.event_name));
        }
        let mana_date = game_date_for_timestamp(request.now as f64)
            .map_err(|error| DigEventRuntimeError::Splash(error.to_string()))?;
        let destination = if splash.mode == "steal" {
            HostileDestination::Player
        } else {
            HostileDestination::Burn
        };
        let losses = victim_ids
            .iter()
            .map(|victim_id| HostileLossRequest {
                victim_id: *victim_id,
                guild_id: request.guild_id,
                requested: amount,
                kind: format!("dig_splash_{}", splash.mode),
                actor_id: Some(request.discord_id),
                event_key: format!("{}:splash:{victim_id}", request.event_key),
                destination,
                recipient_id: (destination == HostileDestination::Player)
                    .then_some(request.discord_id),
                clamp_to_balance: destination == HostileDestination::Burn,
                min_balance: Some(HOSTILE_LOSS_MIN_BALANCE),
                protect_self: false,
                metadata: json!({
                    "event_name": resolution.event_name,
                    "strategy": splash.strategy,
                    "mode": splash.mode,
                }),
                occurred_at: request.now,
                mana_date: mana_date.clone(),
            })
            .collect::<Vec<_>>();
        let settlements = ShopRuntimeRepository::new(&self.path)
            .apply_hostile_losses(&losses)
            .map_err(|error| DigEventRuntimeError::Splash(error.to_string()))?;
        let audit = DigEventRuntimeRepository::new(&self.path);
        let mut victims = Vec::new();
        let mut absorbed_total = 0_i64;
        let mut shielded_count = 0_usize;
        let mut actor_transfer = 0_i64;
        for (loss, settlement) in losses.iter().zip(settlements) {
            let Ok(settlement) = settlement else {
                continue;
            };
            if settlement.absorbed > 0 {
                absorbed_total = absorbed_total.saturating_add(settlement.absorbed);
                shielded_count += 1;
            }
            if settlement.applied <= 0 {
                continue;
            }
            if destination == HostileDestination::Player && !settlement.duplicate {
                actor_transfer = actor_transfer.saturating_add(settlement.applied);
            }
            let detail = splash_detail(resolution, splash, settlement.applied, amount);
            audit.record_hostile_splash_audit(DigHostileSplashAuditRequest {
                guild_id: Some(request.guild_id),
                digger_id: request.discord_id,
                victim_id: loss.victim_id,
                event_name: &resolution.event_name,
                strategy: &splash.strategy,
                mode: &splash.mode,
                event_key: &loss.event_key,
                amount: settlement.applied,
                detail_json: &detail,
                created_at: request.now,
            })?;
            victims.push((loss.victim_id, settlement.applied));
        }
        let mut execution = SplashExecution::from_victims(
            splash,
            &resolution.event_name,
            victims,
            absorbed_total,
            shielded_count,
        );
        execution.actor_transfer = actor_transfer;
        Ok(execution)
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_quest_followup(
        &self,
        request: DigRuntimeEventRequest<'_>,
        repository: &DigEventRuntimeRepository,
        quest_snapshot: &cama_db::dig_event_runtime::DigEventQuestSnapshot,
        resolution: &CanonicalEventResolution,
        depth_after: i64,
        prestige_level: i64,
        actor_applied_now: bool,
        entropy: &mut impl LootEntropy,
    ) -> Option<DigEventQuestFinale> {
        if resolution.quest_id.is_none()
            || resolution.choice != "desperate"
            || !resolution.succeeded
        {
            return None;
        }
        let mut predicates = BTreeSet::new();
        if quest_snapshot.recent_bet {
            predicates.insert("bet_within_7d".to_owned());
        }
        // Completion intentionally commits before its finale. A duplicate
        // actor delivery resumes that idempotent follow-up after a process
        // stop in the visible completion/reward window.
        if !actor_applied_now
            && let Some((quest, event_step)) = canonical_quest_for_event(request.event_id)
            && event_step == quest.step_event_ids.len() as i64
            && quest_snapshot.completed_quest_ids.contains(&quest.quest_id)
        {
            return self.resolve_and_dispatch_quest_finale(
                request,
                repository,
                &quest.quest_id,
                &quest.finale,
                entropy,
            );
        }
        let progress = advance_canonical_quest_on_desperate_success(
            request.event_id,
            quest_snapshot.active_quest_id.as_deref(),
            quest_snapshot.active_quest_step,
            &quest_snapshot.completed_quest_ids,
            depth_after,
            prestige_level,
            &predicates,
        );
        match progress {
            CanonicalQuestProgress::Noop => None,
            CanonicalQuestProgress::Activated {
                quest_id,
                next_step,
            }
            | CanonicalQuestProgress::Advanced {
                quest_id,
                next_step,
            } => {
                let _ = repository.apply_quest_mutation(
                    DigEventActorKey {
                        discord_id: request.discord_id,
                        guild_id: Some(request.guild_id),
                    },
                    DigEventQuestMutation::SetActive {
                        quest_id: &quest_id,
                        next_step,
                    },
                    request.now,
                );
                None
            }
            CanonicalQuestProgress::Completed { quest_id, finale } => {
                let completion = repository
                    .apply_quest_mutation(
                        DigEventActorKey {
                            discord_id: request.discord_id,
                            guild_id: Some(request.guild_id),
                        },
                        DigEventQuestMutation::Complete {
                            quest_id: &quest_id,
                        },
                        request.now,
                    )
                    .ok()?;
                if !completion.applied_now {
                    return self.resolve_and_dispatch_quest_finale(
                        request, repository, &quest_id, &finale, entropy,
                    );
                }
                self.resolve_and_dispatch_quest_finale(
                    request, repository, &quest_id, &finale, entropy,
                )
            }
        }
    }

    fn resolve_and_dispatch_quest_finale(
        &self,
        request: DigRuntimeEventRequest<'_>,
        repository: &DigEventRuntimeRepository,
        quest_id: &str,
        finale: &CanonicalQuestFinale,
        entropy: &mut impl LootEntropy,
    ) -> Option<DigEventQuestFinale> {
        let roll_count = match finale {
            CanonicalQuestFinale::RelicGrant { roll_count, .. } => *roll_count,
            CanonicalQuestFinale::JcPlusGuildModifier { .. } => 0,
        };
        let rolls = (0..roll_count).map(|_| entropy.unit()).collect::<Vec<_>>();
        let finale = resolve_canonical_quest_finale(finale, &rolls).ok()?;
        self.dispatch_quest_finale(request, repository, quest_id, finale)
            .ok()
    }

    fn dispatch_quest_finale(
        &self,
        request: DigRuntimeEventRequest<'_>,
        repository: &DigEventRuntimeRepository,
        quest_id: &str,
        finale: CanonicalQuestFinaleOutcome,
    ) -> Result<DigEventQuestFinale, DigEventRuntimeError> {
        let key = DigEventActorKey {
            discord_id: request.discord_id,
            guild_id: Some(request.guild_id),
        };
        match finale {
            CanonicalQuestFinaleOutcome::JcPlusGuildModifier {
                personal_jc,
                modifier_id,
                duration_seconds,
                modifier_payload,
            } => {
                let scaled = scale_positive_dig_jc(personal_jc);
                let economy =
                    SqliteEconomyEventService::new(&self.path, self.config.economy_event.clone());
                let daily = economy
                    .adjust_reward_at(request.guild_id, scaled, request.now)
                    .unwrap_or(scaled);
                let hooked = self
                    .finale_tax_hook
                    .map_or(daily, |hook| hook(daily).max(0));
                let net_jc = self.apply_finale_mana_taxes(request, hooked, quest_id);
                let receipt = (net_jc > 0)
                    .then(|| {
                        repository.settle_finale_jc_atomic(DigEventFinaleJcRequest {
                            key,
                            quest_id,
                            event_key: &format!("{}:quest-finale-jc", request.event_key),
                            gross_jc: personal_jc,
                            net_jc,
                            detail_json: &json!({
                                "personal_jc_gross": personal_jc,
                                "reward_multiplier": 0.65,
                            })
                            .to_string(),
                            now: request.now,
                        })
                    })
                    .transpose()?;
                let modifier = if modifier_id.is_empty() || duration_seconds <= 0 {
                    None
                } else {
                    repository
                        .set_guild_modifier(DigEventGuildModifierRequest {
                            guild_id: Some(request.guild_id),
                            actor_id: request.discord_id,
                            modifier_id: &modifier_id,
                            duration_seconds,
                            payload_json: &modifier_payload.to_string(),
                            event_key: &format!("{}:quest-finale-modifier", request.event_key),
                            now: request.now,
                        })
                        .ok()
                        .map(|receipt| DigEventGuildModifierOutcome {
                            modifier_id,
                            duration_seconds,
                            payload: modifier_payload,
                            expires_at: receipt.expires_at,
                            applied_now: receipt.applied_now,
                        })
                };
                Ok(DigEventQuestFinale::JcAndModifier {
                    quest_id: quest_id.to_owned(),
                    gross_jc: personal_jc,
                    net_jc,
                    balance_after: receipt.and_then(|receipt| receipt.balance_after),
                    modifier,
                })
            }
            CanonicalQuestFinaleOutcome::RelicGrant {
                relic_name,
                artifact_id,
                relic_stat_ids,
            } => {
                let receipt = repository.grant_finale_relic_atomic(DigEventFinaleRelicRequest {
                    key,
                    quest_id,
                    event_key: &format!("{}:quest-finale-relic", request.event_key),
                    artifact_id: &artifact_id,
                    detail_json: &json!({
                        "relic_name": relic_name,
                        "relic_stat_ids": relic_stat_ids,
                    })
                    .to_string(),
                    now: request.now,
                })?;
                Ok(DigEventQuestFinale::Relic {
                    quest_id: quest_id.to_owned(),
                    relic_name,
                    artifact_id,
                    relic_stat_ids,
                    reward_row_id: receipt.reward_row_id,
                })
            }
        }
    }

    fn apply_finale_mana_taxes(
        &self,
        request: DigRuntimeEventRequest<'_>,
        amount: i64,
        quest_id: &str,
    ) -> i64 {
        if amount <= 0 {
            return amount;
        }
        let Ok(today) = game_date_for_timestamp(request.now as f64) else {
            return amount;
        };
        let effects = ManaRepository::new(&self.path)
            .get_mana(request.discord_id, Some(request.guild_id))
            .ok()
            .flatten()
            .filter(|mana| mana.assigned_date == today && !mana.consumed_today)
            .map(|mana| mana_effects_for_land(&mana.current_land))
            .unwrap_or_default();
        let mut modified = amount;
        if effects.plains_tithe_rate > 0.0 {
            let tithe = ((modified as f64 * effects.plains_tithe_rate) as i64).max(1);
            let context = LedgerContext {
                source: Some("dig".to_owned()),
                actor_id: Some(request.discord_id),
                related_type: Some("plains_tithe".to_owned()),
                related_id: Some(quest_id.to_owned()),
                reason: Some("dig plains tithe reserve credit".to_owned()),
                metadata: Some(json!({"total_jc": amount, "tithe": tithe}).to_string()),
            };
            if LoanRepository::new(&self.path)
                .add_to_nonprofit_fund(Some(request.guild_id), tithe, Some(&context))
                .is_ok()
            {
                modified = modified.saturating_sub(tithe);
            }
        }
        if effects.blue_tax_rate > 0.0 && modified > 0 {
            let tax = ((modified as f64 * effects.blue_tax_rate) as i64).max(1);
            modified = modified.saturating_sub(tax);
        }
        modified.max(0)
    }
}

fn event_delivery_from_db(
    delivery: DbDigEventDeliverySnapshot,
) -> Result<DigEventDeliverySnapshot, DigEventRuntimeError> {
    let mut outcome = serde_json::from_str::<DigEventRuntimeOutcome>(&delivery.outcome_json)
        .map_err(|error| DigEventRuntimeError::Policy(error.to_string()))?;
    // A pending draft is intentionally incomplete and must never be rendered
    // or delivered. Ready/Delivered rows are frozen full outcomes; retain the
    // action id fallback only for legacy rows written before the outbox knew
    // the AUTOINCREMENT value.
    if delivery.state != DbDigEventDeliveryState::Pending && outcome.action_id.is_none() {
        outcome.action_id = Some(delivery.action_id);
    }
    let state = match delivery.state {
        DbDigEventDeliveryState::Pending => DigEventDeliveryState::Pending,
        DbDigEventDeliveryState::Ready => DigEventDeliveryState::Ready,
        DbDigEventDeliveryState::Delivered => DigEventDeliveryState::Delivered,
    };
    Ok(DigEventDeliverySnapshot {
        action_id: delivery.action_id,
        source_key: delivery.source_key,
        discord_id: delivery.discord_id,
        guild_id: delivery.guild_id,
        context: DigEventDeliveryContext::new(
            delivery.discord_id,
            delivery.guild_id,
            delivery.interaction_id,
            delivery.channel_id,
        ),
        committed_at: delivery.committed_at,
        state,
        outcome,
        delivered_at: delivery.delivered_at,
    })
}

/// Build the exact event prompt from values already captured by a Dig
/// transaction. Durable delivery can call this before commit instead of
/// re-reading a newer tunnel after a crash or duplicate interaction.
pub fn canonical_event_action_presentation(
    event_id: &str,
    luminosity: i64,
    prestige_perks_json: &str,
    dig_action_id: i64,
) -> Result<DigEventActionPresentation, DigEventRuntimeError> {
    let event = canonical_event_presentation(event_id).map_err(DigEventRuntimeError::Policy)?;
    let authored = canonical_event(event_id)
        .ok_or_else(|| DigEventRuntimeError::Policy("Unknown event.".to_owned()))?;
    let perks = serde_json::from_str::<Vec<String>>(prestige_perks_json).unwrap_or_default();
    let reading_the_stone_hint = perks
        .iter()
        .any(|perk| perk == "reading_the_stone")
        .then(|| {
            reading_the_stone_hint(authored, usize::try_from(dig_action_id).unwrap_or_default())
        })
        .flatten()
        .map(str::to_owned);
    Ok(DigEventActionPresentation {
        event,
        luminosity,
        safe_disabled: luminosity <= 0 && authored.risky_option.is_some(),
        reading_the_stone_hint,
    })
}

#[derive(Clone, Debug)]
struct AuthoredModifier {
    modifier_id: String,
    duration_seconds: i64,
    payload: Value,
}

impl AuthoredModifier {
    fn with_receipt(self, receipt: DigEventGuildModifierReceipt) -> DigEventGuildModifierOutcome {
        DigEventGuildModifierOutcome {
            modifier_id: self.modifier_id,
            duration_seconds: self.duration_seconds,
            payload: self.payload,
            expires_at: receipt.expires_at,
            applied_now: receipt.applied_now,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct SplashExecution {
    outcome: Option<DigEventSplashOutcome>,
    actor_transfer: i64,
}

impl SplashExecution {
    fn empty(splash: &CanonicalSplash, event_name: &str) -> Self {
        Self::from_victims(splash, event_name, Vec::new(), 0, 0)
    }

    fn from_victims(
        splash: &CanonicalSplash,
        event_name: &str,
        victims: Vec<(i64, i64)>,
        absorbed_total: i64,
        shielded_count: usize,
    ) -> Self {
        let total_moved = victims.iter().map(|(_, amount)| *amount).sum();
        Self {
            outcome: Some(DigEventSplashOutcome {
                strategy: splash.strategy.clone(),
                event_name: event_name.to_owned(),
                victims,
                total_moved,
                mode: splash.mode.clone(),
                absorbed_total,
                shielded_count,
            }),
            actor_transfer: 0,
        }
    }
}

#[derive(Clone, Debug)]
enum PreparedReward {
    Gear {
        item_id: String,
        slot: &'static str,
        tier: i64,
        durability: i64,
    },
    Consumable(String),
    Artifact {
        artifact_id: String,
        is_relic: bool,
    },
}

impl PreparedReward {
    fn as_db<'a>(&'a self, source: &'a str) -> cama_db::dig_event_runtime::DigEventReward<'a> {
        match self {
            Self::Gear {
                item_id,
                slot,
                tier,
                durability,
            } => cama_db::dig_event_runtime::DigEventReward::Gear {
                item_id,
                slot,
                tier: *tier,
                durability: *durability,
                source,
            },
            Self::Consumable(item_type) => {
                cama_db::dig_event_runtime::DigEventReward::Consumable { item_type }
            }
            Self::Artifact {
                artifact_id,
                is_relic,
            } => cama_db::dig_event_runtime::DigEventReward::Artifact {
                artifact_id,
                is_relic: *is_relic,
            },
        }
    }
}

fn event_policy(
    snapshot: &DigEventActorSnapshot,
    chained: bool,
    _minigame_scale: f64,
) -> CanonicalEventPolicy {
    let ascension = ascension_effects(snapshot.prestige_level as i32);
    let number = |key: &str| {
        ascension
            .get(key)
            .and_then(|effect| effect.number())
            .unwrap_or(0.0)
    };
    let boolean = |key: &str| {
        ascension
            .get(key)
            .and_then(|effect| effect.boolean())
            .unwrap_or(false)
    };
    let perks =
        serde_json::from_str::<Vec<String>>(&snapshot.prestige_perks_json).unwrap_or_default();
    let perk_count = |perk: &str| perks.iter().filter(|owned| *owned == perk).count() as f64;
    CanonicalEventPolicy {
        luminosity: snapshot.luminosity,
        current_streak: snapshot.streak_days,
        current_depth: snapshot.depth,
        prestige_level: snapshot.prestige_level,
        next_boss_boundary: next_boss_boundary(&snapshot.boss_progress_json),
        active_curse_remaining: active_curse_remaining(snapshot.temp_curse_json.as_deref()),
        chained,
        diviners_knot: snapshot.equipped_relics.contains("diviners_knot"),
        surveyors_loop: snapshot.equipped_gear.contains("surveyors_loop"),
        ruinwager_edge: snapshot.equipped_gear.contains("ruinwager_signet"),
        black_wax_seal: snapshot.equipped_relics.contains("black_wax_seal"),
        active_temp_curse: snapshot.temp_curse_json.is_some(),
        cruel_safe_failure: number("cruel_safe_fail"),
        burning_ledger: snapshot.equipped_relics.contains("burning_ledger"),
        chain_jc_multiplier: number("chain_jc_multiplier").max(1.0),
        expedition_reward_bonus: perk_count("tunnel_mastery") * 0.50,
        risky_success_bonus: perk_count("veteran_miner") * 0.05,
        chipped_compass: snapshot.equipped_relics.contains("chipped_compass"),
        event_chain_enabled: boolean("event_chain"),
        // Application applies central/daily/Dig economy after protected
        // splash so burn-success payout can use the actual destroyed amount.
        minigame_jc_delta_scale: 1.0,
        economy_reward_multiplier: 1.0,
    }
}

fn reading_the_stone_hint(
    event: &crate::dig_loot::CanonicalEventDef,
    variant: usize,
) -> Option<&'static str> {
    let mut best = None::<(&'static [&'static str; 3], f64)>;
    for (option, hints) in [
        (event.safe_option.as_ref(), &READING_THE_STONE_SAFE_HINTS),
        (event.risky_option.as_ref(), &READING_THE_STONE_RISKY_HINTS),
        (
            event.desperate_option.as_ref(),
            &READING_THE_STONE_DESPERATE_HINTS,
        ),
    ] {
        let Some(option) = option else {
            continue;
        };
        let success = option.success.jc as f64;
        let failure = option
            .failure
            .as_ref()
            .map_or(0.0, |outcome| outcome.jc as f64);
        let expected = option.success_chance * success + (1.0 - option.success_chance) * failure;
        if best.is_none_or(|(_, best_expected)| expected > best_expected) {
            best = Some((hints, expected));
        }
    }
    best.map(|(hints, _)| hints[variant % hints.len()])
}

fn resolution_as_outcome(
    resolution: &CanonicalEventResolution,
) -> crate::dig_loot::CanonicalEventOutcome {
    crate::dig_loot::CanonicalEventOutcome {
        description: resolution.message.clone(),
        advance: resolution.advance,
        jc: resolution.economy_gross_jc,
        cave_in: resolution.cave_in,
        streak_loss: resolution.streak_loss,
        curse: resolution.curse.clone(),
        gear_reward_pool: resolution.gear_reward_pool.clone(),
        consumable_reward_pool: resolution.consumable_reward_pool.clone(),
        artifact_reward_pool: resolution.artifact_reward_pool.clone(),
    }
}

fn event_seed(request: DigRuntimeEventRequest<'_>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in request
        .event_key
        .bytes()
        .chain(request.event_id.bytes())
        .chain(request.choice.bytes())
        .chain(request.discord_id.to_le_bytes())
        .chain(request.guild_id.to_le_bytes())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

fn legacy_event_seed(request: DigLegacyCatalogRequest<'_>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in request
        .event_key
        .bytes()
        .chain(request.event_id.bytes())
        .chain(request.choice.bytes())
        .chain(request.discord_id.to_le_bytes())
        .chain(request.guild_id.to_le_bytes())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

fn authored_modifier(value: &Value) -> Option<AuthoredModifier> {
    Some(AuthoredModifier {
        modifier_id: value.get("id")?.as_str()?.to_owned(),
        duration_seconds: value.get("duration_seconds")?.as_i64()?,
        payload: value.get("payload").cloned().unwrap_or_else(|| json!({})),
    })
}

fn victim_strategy(strategy: &str) -> Option<EncounterVictimStrategy> {
    match strategy {
        "richest_n" => Some(EncounterVictimStrategy::RichestN),
        "active_diggers" => Some(EncounterVictimStrategy::ActiveDiggers),
        "random_active" => Some(EncounterVictimStrategy::RandomActive),
        "deepest_n" => Some(EncounterVictimStrategy::DeepestN),
        _ => None,
    }
}

fn candidate_window(
    strategy: EncounterVictimStrategy,
    count: usize,
    now: i64,
) -> (i64, Option<usize>) {
    match strategy {
        EncounterVictimStrategy::RandomActive => {
            (now.saturating_sub(RANDOM_ACTIVE_LOOKBACK_SECONDS), None)
        }
        EncounterVictimStrategy::ActiveDiggers => (
            now.saturating_sub(ACTIVE_DIGGER_LOOKBACK_SECONDS),
            Some((count * 4).max(count + 8)),
        ),
        EncounterVictimStrategy::RichestN | EncounterVictimStrategy::DeepestN => (0, Some(count)),
    }
}

fn select_victims(
    mut candidates: Vec<i64>,
    count: usize,
    strategy: EncounterVictimStrategy,
    entropy: &mut impl LootEntropy,
) -> Vec<i64> {
    if !strategy.uses_random_sample() {
        candidates.truncate(count);
        return candidates;
    }
    let mut selected = Vec::with_capacity(count.min(candidates.len()));
    while !candidates.is_empty() && selected.len() < count {
        let index = entropy.choose_index(candidates.len());
        selected.push(candidates.swap_remove(index));
    }
    selected
}

fn persisted_buff_json(buff: &crate::dig_loot::CanonicalTempBuff) -> String {
    json!({
        "id": buff.id,
        "name": buff.name,
        "digs_remaining": buff.duration_digs,
        "effect": buff.effect,
    })
    .to_string()
}

fn persisted_curse_json(curse: &crate::dig_loot::CanonicalTempCurse) -> String {
    json!({
        "id": curse.id,
        "name": curse.name,
        "digs_remaining": curse.duration_digs,
        "effect": curse.effect,
    })
    .to_string()
}

fn event_curse_replacement(
    snapshot: &DigEventActorSnapshot,
    resolution: &CanonicalEventResolution,
) -> Option<String> {
    if let Some(curse) = &resolution.persisted_curse {
        return Some(persisted_curse_json(curse));
    }
    if !resolution.black_wax_seal_spent {
        return None;
    }
    let remaining = resolution.active_curse_remaining_after?;
    if remaining <= 0 || resolution.curse_cleared {
        return Some(String::new());
    }
    let mut curse =
        serde_json::from_str::<Value>(snapshot.temp_curse_json.as_deref().unwrap_or("{}"))
            .unwrap_or_else(|_| json!({}));
    curse["digs_remaining"] = Value::from(remaining);
    Some(curse.to_string())
}

fn prepared_reward(
    resolution: &CanonicalEventResolution,
) -> Result<Option<PreparedReward>, DigEventRuntimeError> {
    let Some(reward) = &resolution.reward else {
        return Ok(None);
    };
    match reward {
        CanonicalReward::Gear(item_id) => {
            let (slot, tier, durability) = canonical_gear_row(item_id).ok_or_else(|| {
                DigEventRuntimeError::Policy(format!("Unknown event gear reward {item_id}."))
            })?;
            Ok(Some(PreparedReward::Gear {
                item_id: item_id.clone(),
                slot,
                tier,
                durability,
            }))
        }
        CanonicalReward::Consumable(item_id) => {
            Ok(Some(PreparedReward::Consumable(item_id.clone())))
        }
        CanonicalReward::Artifact(artifact_id) => {
            let definition = artifact_catalog()
                .into_iter()
                .find(|artifact| artifact.id == artifact_id)
                .ok_or_else(|| {
                    DigEventRuntimeError::Policy(format!(
                        "Unknown event artifact reward {artifact_id}."
                    ))
                })?;
            Ok(Some(PreparedReward::Artifact {
                artifact_id: artifact_id.clone(),
                is_relic: definition.is_relic,
            }))
        }
    }
}

fn canonical_gear_row(item_id: &str) -> Option<(&'static str, i64, i64)> {
    match item_id {
        "glassbreaker_pick" => Some(("weapon", 3, 8)),
        "needle_pick" => Some(("weapon", 3, 16)),
        "briarplate" => Some(("armor", 3, 14)),
        "nullweave_mantle" => Some(("armor", 3, 12)),
        "springheel_boots" => Some(("boots", 3, 14)),
        "anchor_boots" => Some(("boots", 3, 16)),
        "loaded_die" => Some(("amulet", 3, 12)),
        "blood_locket" => Some(("amulet", 3, 14)),
        "surveyors_loop" => Some(("ring", 3, 14)),
        "ruinwager_signet" => Some(("ring", 3, 14)),
        "red_thread_band" => Some(("ring", 3, 14)),
        _ => None,
    }
}

fn event_detail(
    resolution: &CanonicalEventResolution,
    splash: Option<&DigEventSplashOutcome>,
) -> String {
    json!({
        "succeeded": resolution.succeeded,
        "advance": resolution.advance,
        "jc": resolution.jc,
        "cave_in": resolution.cave_in,
        "gross_jc": (resolution.jc > 0).then_some(resolution.economy_gross_jc),
        "reward_multiplier": (resolution.jc > 0).then_some(0.65),
        "gear": reward_id_for_kind(&resolution.reward, RewardKind::Gear),
        "consumable": reward_id_for_kind(&resolution.reward, RewardKind::Consumable),
        "artifact": reward_id_for_kind(&resolution.reward, RewardKind::Artifact),
        "streak_days_lost": (resolution.streak_loss > 0).then_some(resolution.streak_loss),
        "curse": resolution.curse.as_ref().map(|curse| curse.name.as_str()),
        "splash_victims": splash.map(|splash| splash.victims.clone()),
        "black_wax_seal_spent": resolution.black_wax_seal_spent,
        "splash_payout_ratio": resolution.splash_payout_ratio,
    })
    .to_string()
}

#[derive(Clone, Copy)]
enum RewardKind {
    Gear,
    Consumable,
    Artifact,
}

fn reward_id_for_kind(reward: &Option<CanonicalReward>, kind: RewardKind) -> Option<&str> {
    match (reward, kind) {
        (Some(CanonicalReward::Gear(id)), RewardKind::Gear)
        | (Some(CanonicalReward::Consumable(id)), RewardKind::Consumable)
        | (Some(CanonicalReward::Artifact(id)), RewardKind::Artifact) => Some(id),
        _ => None,
    }
}

fn splash_detail(
    resolution: &CanonicalEventResolution,
    splash: &CanonicalSplash,
    amount: i64,
    gross_jc: i64,
) -> String {
    json!({
        "event_id": resolution.event_id,
        "penalty_requested": splash.penalty_jc,
        "penalty_scaled": amount,
        "gross_jc": (splash.mode == "grant").then_some(gross_jc),
        "reward_multiplier": (splash.mode == "grant").then_some(0.65),
    })
    .to_string()
}

fn resolve_chain(
    resolution: &CanonicalEventResolution,
    depth_after: i64,
    prestige_level: i64,
    entropy: &mut impl LootEntropy,
) -> Option<&'static crate::dig_loot::CanonicalEventDef> {
    let deterministic = canonical_chain_event(
        &resolution.event_id,
        depth_after,
        prestige_level,
        None,
        None,
    );
    if deterministic.is_some() {
        // Catalog entries are process-static; make the inferred lifetime
        // explicit through the canonical lookup return value.
        return deterministic;
    }
    if prestige_level < 7 {
        return None;
    }
    let chain_roll = entropy.unit();
    let selection_roll = (chain_roll < CANONICAL_EVENT_CHAIN_CHANCE).then(|| entropy.unit());
    canonical_chain_event(
        &resolution.event_id,
        depth_after,
        prestige_level,
        Some(chain_roll),
        selection_roll,
    )
}

fn active_curse_remaining(raw: Option<&str>) -> Option<i64> {
    raw.and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.get("digs_remaining").and_then(Value::as_i64))
}

fn event_view_expired(created_at: i64, now: i64) -> bool {
    now.saturating_sub(created_at) >= DIG_EVENT_VIEW_TIMEOUT_SECONDS
}

fn next_boss_boundary(raw: &str) -> Option<i64> {
    let progress = serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let status = |boundary: i64| {
        progress.get(&boundary.to_string()).and_then(|value| {
            value
                .as_str()
                .or_else(|| value.get("status").and_then(Value::as_str))
        })
    };
    for boundary in [25, 50, 75, 100, 150, 200, 275] {
        if status(boundary) != Some("defeated") {
            return Some(boundary);
        }
    }
    (status(350) != Some("defeated")).then_some(350)
}

fn mana_effects_for_land(land: &str) -> ManaEffects {
    let color = match land {
        "Mountain" => Some("Red"),
        "Island" => Some("Blue"),
        "Forest" => Some("Green"),
        "Plains" => Some("White"),
        "Swamp" => Some("Black"),
        _ => None,
    };
    ManaEffects::for_color(color, color.map(|_| land))
}

#[cfg(test)]
#[path = "dig_event_runtime/tests.rs"]
mod tests;
