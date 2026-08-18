use super::*;

/// The user-facing weather portion of a live Dig result.  Keep this separate
/// from the persisted forecast row and its mechanical effects so the result
/// remains a stable presentation snapshot even when the daily forecast is
/// subsequently queried or repaired.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigRuntimeWeatherInfo {
    pub name: String,
    pub description: String,
}

/// Discord context captured before the Dig transaction commits.  The
/// delivery outbox stores this immutable projection so a restart can recover
/// a public message without depending on a live interaction object.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigRuntimeDeliveryContext {
    pub interaction_id: u64,
    pub channel_id: i64,
    pub display_name: String,
    pub avatar_url: Option<String>,
}

/// Immutable delivery inputs captured before the actor transaction. The
/// SQLite adapter persists the resulting projection beside the action row;
/// lightweight stores use the existing attach hook as a compatibility path.
#[derive(Clone, Debug)]
pub struct DigRuntimeDeliveryDraft {
    pub discord_id: i64,
    pub guild_id: i64,
    pub outcome: DigRuntimeOutcome,
    pub context: DigRuntimeDeliveryContext,
    pub committed_at: i64,
}

impl DigRuntimeDeliveryContext {
    #[must_use]
    pub fn new(
        interaction_id: u64,
        channel_id: i64,
        display_name: impl Into<String>,
        avatar_url: Option<String>,
    ) -> Self {
        Self {
            interaction_id,
            channel_id,
            display_name: display_name.into(),
            avatar_url,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DigRuntimeRenderKind {
    First,
    Normal,
    Boss,
    Event,
}

impl DigRuntimeRenderKind {
    #[must_use]
    pub const fn requires_event_part(self) -> bool {
        matches!(self, Self::Event)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DigRuntimeEventKind {
    Simple,
    Choice,
    Boon,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigRuntimeEventRenderSnapshot {
    pub event_id: String,
    pub description: String,
    pub ascii_art: Option<String>,
    pub boon_names: Vec<String>,
    pub reading_the_stone_hint: Option<String>,
    pub safe_disabled: bool,
    pub safe_label: Option<String>,
    pub risky_label: Option<String>,
    pub desperate_label: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigRuntimeBossRenderSnapshot {
    pub boundary: i64,
    pub boss_id: String,
    pub boss_name: String,
    pub dialogue: String,
    pub is_pinnacle: bool,
    pub phase: i64,
    pub wager_allowed: bool,
    pub carried_wager: i64,
    pub has_scout_lantern: bool,
    pub luminosity: i64,
    pub encounter_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigRuntimeRenderSnapshot {
    pub kind: DigRuntimeRenderKind,
    pub title: String,
    pub description: String,
    pub layer_color: u32,
    pub depth_transition: String,
    pub layer_name: String,
    pub flavor_narrative: Option<String>,
    pub footer: Option<String>,
    pub boss_boundary_copy: Option<String>,
    pub consumed_copy: Option<String>,
    pub artifact_name: Option<String>,
    pub relic_trim_notice_copy: Option<String>,
    pub event_kind: Option<DigRuntimeEventKind>,
    pub event: Option<DigRuntimeEventRenderSnapshot>,
    pub layer_media_key: String,
    pub pickaxe_tier: i64,
    pub item_media_keys: Vec<String>,
    #[serde(default)]
    pub weather: Option<DigRuntimeWeatherInfo>,
    pub boss: Option<DigRuntimeBossRenderSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DigRuntimeFlavorSnapshot {
    Pending,
    Applied {
        narrative: Option<String>,
        tone: Option<String>,
        callback_reference: Option<String>,
        npc_id_or_name: Option<String>,
        npc_line: Option<String>,
        picked_event_id: Option<String>,
        bonus_delta: i64,
    },
    Skipped,
}

impl DigRuntimeFlavorSnapshot {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Applied { .. } | Self::Skipped)
    }

    pub(super) fn narrative(&self) -> Option<&str> {
        match self {
            Self::Applied { narrative, .. } => narrative.as_deref(),
            Self::Pending | Self::Skipped => None,
        }
    }
}

/// Durable state for the post-Dig Blood Pact effect.
///
/// This state lives inside the immutable delivery projection rather than in a
/// second queue table.  `Pending` is the crash window between the committed
/// Dig action and the post-commit effect; once the repository boundary has
/// returned, the state is terminal and retries only reconcile the projection.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum DigRuntimeBloodPactSnapshot {
    #[default]
    Pending,
    Applied {
        skimmed: i64,
    },
    Skipped,
}

impl DigRuntimeBloodPactSnapshot {
    #[must_use]
    pub const fn for_outcome(outcome: &DigRuntimeOutcome) -> Self {
        if outcome.cave_in || outcome.jc_earned <= 0 {
            Self::Skipped
        } else {
            Self::Pending
        }
    }

    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Applied { .. } | Self::Skipped)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigRuntimeDeliverySnapshot {
    pub action_id: i64,
    pub source_key: String,
    pub discord_id: i64,
    pub guild_id: i64,
    pub committed_at: i64,
    pub context: DigRuntimeDeliveryContext,
    pub outcome: DigRuntimeOutcome,
    pub render: DigRuntimeRenderSnapshot,
    pub flavor: DigRuntimeFlavorSnapshot,
    /// Durable post-commit economy effect.  The serde default keeps delivery
    /// rows written before Blood Pact admission recoverable as `Pending`.
    #[serde(default)]
    pub blood_pact: DigRuntimeBloodPactSnapshot,
    pub main_delivered_at: Option<i64>,
    pub event_delivered_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DigRuntimeDeliveryPart {
    Main,
    Event,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeExecution {
    pub outcome: DigRuntimeOutcome,
    pub delivery: Option<DigRuntimeDeliverySnapshot>,
}

impl Deref for DigRuntimeExecution {
    type Target = DigRuntimeOutcome;

    fn deref(&self) -> &Self::Target {
        &self.outcome
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigRuntimePendingDeliveryQuery {
    pub guild_id: Option<i64>,
    pub discord_id: Option<i64>,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeMarkDelivered {
    pub action_id: i64,
    pub source_key: String,
    pub delivered_at: i64,
    pub part: DigRuntimeDeliveryPart,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeRebindDeliveryChannel {
    pub action_id: i64,
    pub source_key: String,
    pub part: DigRuntimeDeliveryPart,
    pub expected_channel_id: i64,
    pub fallback_channel_id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeFinalizeDelivery {
    pub action_id: i64,
    pub source_key: String,
    pub flavor: DigRuntimeFlavorSnapshot,
    pub boss: Option<DigRuntimeBossRenderSnapshot>,
}

/// Request to settle the durable post-Dig Blood Pact effect for one delivery.
/// The delivery row, rather than the caller, supplies the actor, guild, and
/// original immutable earning.  `occurred_at` is retained so retries use the
/// same protection/mana date inputs while reconstructing the effect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeSettleBloodPact {
    pub action_id: i64,
    pub source_key: String,
    pub occurred_at: i64,
}

/// Typed read models used by the Discord transport.  Keeping these beside
/// the aggregate prevents provider code from becoming a second SQL read
/// model implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeTunnelInfo {
    pub depth: i64,
    pub total_digs: i64,
    pub total_jc_earned: i64,
    pub last_dig_at: Option<i64>,
    pub pickaxe_tier: i64,
    pub prestige_level: i64,
    pub luminosity: i64,
    pub hard_hat_charges: i64,
    pub tunnel_name: String,
    pub route_state: Option<String>,
}

/// Canonical `/dig flex` projection. Discord copy and entropy stay in the
/// transport, while persisted boss/title normalization remains here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeFlexData {
    pub tunnel_name: String,
    pub depth: i64,
    pub total_digs: i64,
    pub total_jc_earned: i64,
    pub prestige_level: i64,
    pub prestige_emoji: String,
    pub titles: Vec<String>,
    pub streak: i64,
    pub layer: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigRuntimeWeatherPresentation {
    pub layer: String,
    pub name: String,
    pub description: String,
    pub effects: DigWeatherEffects,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeLeaderboardRow {
    pub name: String,
    pub depth: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeHallOfFameRow {
    pub name: String,
    pub user_id: i64,
    pub score: i64,
    pub prestige: i64,
}

/// Outcome of an administrator mutation that requires an existing tunnel.
///
/// Keeping the missing-tunnel case distinct prevents Discord adapters from
/// reporting a successful maintenance action when SQLite updated no rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigAdminMutationOutcome {
    Applied,
    MissingTunnel,
}

/// Typed result for a staged inventory, event, or route action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeActionResult {
    pub success: bool,
    pub error: Option<String>,
    pub item: Option<String>,
    pub item_id: Option<i64>,
    pub route_id: Option<String>,
    pub cost: i64,
    pub queued: bool,
    pub balance_after: i64,
    pub action_id: Option<i64>,
}

/// Durable identity and policy inputs for one event component interaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigRuntimeEventRequest<'a> {
    pub discord_id: i64,
    pub guild_id: i64,
    pub event_id: &'a str,
    pub choice: &'a str,
    pub event_key: &'a str,
    pub now: i64,
    pub chained: bool,
}

/// Full typed event result retained through Discord presentation.
#[derive(Clone, Debug, PartialEq)]
pub struct DigRuntimeEventOutcome {
    pub success: bool,
    pub error: Option<String>,
    pub resolution: Option<CanonicalEventResolution>,
    pub depth_before: i64,
    pub depth_after: i64,
    pub balance_after: i64,
    pub action_id: Option<i64>,
    pub reward_row_id: Option<i64>,
    pub applied_now: bool,
}

pub(super) fn build_delivery_snapshot(
    outcome: &DigRuntimeOutcome,
    discord_id: i64,
    guild_id: i64,
    context: DigRuntimeDeliveryContext,
    committed_at: i64,
) -> Option<DigRuntimeDeliverySnapshot> {
    let action_id = outcome.action_id?;
    let layer = layer_at(outcome.depth_after);
    let (kind, event_kind, event) = if let Some(event_id) = outcome.event_id.as_deref() {
        let authored = crate::dig_loot::canonical_event(event_id)?;
        let description = authored
            .descriptions
            .get(
                usize::try_from(action_id).unwrap_or_default() % authored.descriptions.len().max(1),
            )
            .cloned()
            .unwrap_or_default();
        let event_kind = match authored.complexity {
            crate::dig_loot::EventComplexity::Simple => DigRuntimeEventKind::Simple,
            crate::dig_loot::EventComplexity::Choice
            | crate::dig_loot::EventComplexity::Complex => DigRuntimeEventKind::Choice,
            crate::dig_loot::EventComplexity::Boon => DigRuntimeEventKind::Boon,
        };
        let event = DigRuntimeEventRenderSnapshot {
            event_id: authored.id.clone(),
            description,
            ascii_art: authored.ascii_art.clone(),
            boon_names: authored
                .boon_options
                .iter()
                .map(|boon| boon.name.clone())
                .collect(),
            reading_the_stone_hint: None,
            safe_disabled: false,
            safe_label: authored
                .safe_option
                .as_ref()
                .map(|choice| choice.label.clone()),
            risky_label: authored
                .risky_option
                .as_ref()
                .map(|choice| choice.label.clone()),
            desperate_label: authored
                .desperate_option
                .as_ref()
                .map(|choice| choice.label.clone()),
        };
        (DigRuntimeRenderKind::Event, Some(event_kind), Some(event))
    } else if outcome.boss_boundary.is_some() {
        (DigRuntimeRenderKind::Boss, None, None)
    } else if outcome.first_dig {
        (DigRuntimeRenderKind::First, None, None)
    } else {
        (DigRuntimeRenderKind::Normal, None, None)
    };
    let description = if outcome.first_dig {
        "You've started digging your very own tunnel!\n\nUse `/dig` to advance deeper, `/dig shop` to buy items, and `/dig guide` for a full tutorial.\n\nGood luck, miner! **DIG DUG!**".to_owned()
    } else {
        String::new()
    };
    let artifact_name = outcome.artifact_id.as_deref().map(|artifact_id| {
        crate::dig_loot::artifact_catalog()
            .into_iter()
            .find(|artifact| artifact.id == artifact_id)
            .map_or_else(
                || artifact_id.replace('_', " "),
                |artifact| artifact.name.to_owned(),
            )
    });
    let render = DigRuntimeRenderSnapshot {
        kind,
        title: if outcome.first_dig {
            "Welcome to the Mines!".to_owned()
        } else if kind == DigRuntimeRenderKind::Boss {
            "Boss boundary reached".to_owned()
        } else {
            let standard = format!("{} — Depth {}", outcome.tunnel_name, outcome.depth_after);
            if action_id.rem_euclid(5) == 0 {
                const TITLES: [&str; 5] = [
                    "DIG DUG!",
                    "Dig Dug would be proud.",
                    "Another layer conquered!",
                    "Dig Dug: Underground Champion",
                    "You really dug that!",
                ];
                let index =
                    usize::try_from(action_id.rem_euclid(TITLES.len() as i64)).unwrap_or_default();
                format!("{} — Depth {}", TITLES[index], outcome.depth_after)
            } else {
                standard
            }
        },
        description,
        layer_color: delivery_layer_color(layer.name),
        depth_transition: format!("{} → {}", outcome.depth_before, outcome.depth_after),
        layer_name: layer.name.to_owned(),
        flavor_narrative: None,
        footer: (!outcome.tip.is_empty()).then(|| outcome.tip.clone()),
        boss_boundary_copy: outcome
            .boss_boundary
            .map(|boundary| format!("A boss encounter begins at depth {boundary}.")),
        consumed_copy: (!outcome.items_used.is_empty()).then(|| outcome.items_used.join(", ")),
        artifact_name,
        relic_trim_notice_copy: outcome
            .relic_trim_notice
            .then(|| "Your relic loadout was trimmed to the active capacity.".to_owned()),
        event_kind,
        event,
        layer_media_key: layer.name.to_owned(),
        pickaxe_tier: outcome.pickaxe_tier,
        item_media_keys: outcome.items_used.clone(),
        weather: outcome.weather.clone(),
        boss: None,
    };
    Some(DigRuntimeDeliverySnapshot {
        action_id,
        source_key: format!("dig:{action_id}"),
        discord_id,
        guild_id,
        committed_at,
        context,
        outcome: outcome.clone(),
        render,
        // Even the first-dig copy runs through the flavor service. With AI
        // disabled that service records a durable terminal `Skipped` receipt;
        // omitting the pending phase would lose the audited flavor boundary.
        flavor: DigRuntimeFlavorSnapshot::Pending,
        blood_pact: DigRuntimeBloodPactSnapshot::for_outcome(outcome),
        main_delivered_at: None,
        event_delivered_at: None,
    })
}

fn delivery_layer_color(layer_name: &str) -> u32 {
    match layer_name {
        "Dirt" => 0x8B_45_13,
        "Stone" => 0x80_80_80,
        "Crystal" => 0x00_CE_D1,
        "Magma" => 0xFF_45_00,
        "Abyss" => 0x2F_00_47,
        "Fungal Depths" => 0x7C_FC_00,
        "Frozen Core" => 0x87_CE_EB,
        "The Hollow" => 0x0D_0D_0D,
        _ => 0x8B_45_13,
    }
}
