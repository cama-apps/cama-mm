//! Complete existing-schema application orchestration for `/dig` event views.
//!
//! Discord supplies only actor/guild/event/choice/interaction identity. This
//! service snapshots every policy input, consumes deterministic retry-safe
//! entropy, applies the frozen canonical event policy, preserves Python's
//! modifier -> splash -> actor -> chain -> quest ordering, and returns the
//! entire typed result needed to render or recover the interaction.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cama_db::dig_event_runtime::{
    AtomicDigEventSettlement, DigEventActorKey, DigEventActorSnapshot, DigEventFinaleJcRequest,
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
use serde_json::{Value, json};
use thiserror::Error;

use crate::dig_loot::{
    CANONICAL_EVENT_CHAIN_CHANCE, CanonicalEventPolicy, CanonicalEventPresentation,
    CanonicalEventResolution, CanonicalEventResolutionRequest, CanonicalEventRollContext,
    CanonicalEventRolls, CanonicalQuestFinale, CanonicalQuestFinaleOutcome, CanonicalQuestProgress,
    CanonicalReward, CanonicalSplash, LootEntropy, SeededLootEntropy,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigEventSplashOutcome {
    pub strategy: String,
    pub event_name: String,
    pub victims: Vec<(i64, i64)>,
    pub total_moved: i64,
    pub mode: String,
    pub absorbed_total: i64,
    pub shielded_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigEventGuildModifierOutcome {
    pub modifier_id: String,
    pub duration_seconds: i64,
    pub payload: Value,
    pub expires_at: i64,
    pub applied_now: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq)]
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
        let context = CanonicalEventRollContext {
            depth: snapshot.depth,
            luminosity: snapshot.luminosity,
            prestige_level: snapshot.prestige_level,
            eligible_quest_ids: &eligible_quest_ids,
            include_quest_events,
            in_boss,
            void_bait_active: snapshot
                .temp_buff_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .and_then(|value| {
                    value
                        .get("id")
                        .and_then(Value::as_str)
                        .map(|id| id == "void_bait")
                })
                .unwrap_or(false),
            rare_event_multiplier: 0.0,
            legendary_event_multiplier: 0.0,
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

    pub fn resolve_event(
        &self,
        request: DigRuntimeEventRequest<'_>,
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
            splash: splash_execution.outcome,
            guild_modifier,
            chain_event,
            quest_finale,
        })
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
        let repository = DigTunnelEncounterRepository::new(&self.path);
        let (active_since, limit) = candidate_window(strategy, splash.victim_count, request.now);
        let candidates = repository
            .candidate_ids(EncounterCandidateQuery {
                guild_id: Some(request.guild_id),
                digger_discord_id: request.discord_id,
                strategy,
                active_since,
                limit,
            })
            .map_err(|error| DigEventRuntimeError::Splash(error.to_string()))?;
        let victim_ids = select_victims(candidates, splash.victim_count, strategy, entropy);
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
        let amount = scale_deflationary_minigame_jc_delta(
            strengthen_dig_event_penalty(splash.penalty_jc) as f64,
            self.config.minigame_jc_delta_scale,
        );
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
