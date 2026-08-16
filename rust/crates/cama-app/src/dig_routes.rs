//! Persistent route catalog and application policy for Dig.
//!
//! Route state is transport-independent. Entropy is injected, SQLite writes
//! delegate to the existing-schema repository, and Python remains the live
//! Discord runtime and schema authority during parity work.

use std::fmt::{self, Display, Formatter};

use cama_db::dig_routes_repository::{
    BossOutcomeClaim, DigRouteRepository, DigRouteRepositoryError, RouteChoiceClaim,
    RouteChoiceRequest, RouteTunnelRecord,
};

use crate::dig_service::{BOSS_BOUNDARIES, LAYERS, Layer, PINNACLE_DEPTH};
use crate::dig_tunnels::{EffectSpec, EffectValue, Tunnel};

const fn effect(key: &'static str, value: f64) -> EffectSpec {
    EffectSpec {
        key,
        value: EffectValue::Number(value),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DigRoute {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub layer: Option<&'static str>,
    pub effects: &'static [EffectSpec],
}

const SHORED_PASSAGE_EFFECTS: &[EffectSpec] = &[
    effect("cave_in_bonus", -0.08),
    effect("advance_max_penalty", 1.0),
];
const FRACTURED_SHORTCUT_EFFECTS: &[EffectSpec] = &[
    effect("advance_bonus", 1.0),
    effect("cave_in_bonus", 0.08),
    effect("cave_in_loss_bonus", 2.0),
];
const LANTERN_ROAD_EFFECTS: &[EffectSpec] = &[
    effect("luminosity_drain_reduction", 0.50),
    effect("event_chance_multiplier", -0.25),
    effect("artifact_multiplier", 0.75),
];
const ECHOING_GALLERY_EFFECTS: &[EffectSpec] = &[
    effect("event_chance_multiplier", 0.50),
    effect("cave_in_bonus", 0.04),
    effect("luminosity_drain_multiplier", 0.25),
];

pub const UNIVERSAL_ROUTES: [DigRoute; 4] = [
    DigRoute {
        id: "shored_passage",
        name: "Shored Passage",
        description: "Careful supports cut collapse risk by 8 points, but trim 1 from your maximum advance.",
        layer: None,
        effects: SHORED_PASSAGE_EFFECTS,
    },
    DigRoute {
        id: "fractured_shortcut",
        name: "Fractured Shortcut",
        description: "+1 advance through unstable stone; collapses are 8 points likelier and cost 2 more blocks.",
        layer: None,
        effects: FRACTURED_SHORTCUT_EFFECTS,
    },
    DigRoute {
        id: "lantern_road",
        name: "Lantern Road",
        description: "Light drains 50% slower, but events are 25% scarcer and artifact odds fall by 25%.",
        layer: None,
        effects: LANTERN_ROAD_EFFECTS,
    },
    DigRoute {
        id: "echoing_gallery",
        name: "Echoing Gallery",
        description: "Events are 50% more frequent; collapses rise 4 points and light drains 25% faster.",
        layer: None,
        effects: ECHOING_GALLERY_EFFECTS,
    },
];

const OLD_SUPPORTS_EFFECTS: &[EffectSpec] = &[
    effect("cave_in_bonus", -0.05),
    effect("cave_in_loss_cap", 6.0),
    effect("advance_max_penalty", 1.0),
];
const SURVEYORS_CUT_EFFECTS: &[EffectSpec] = &[
    effect("event_chance_multiplier", 0.25),
    effect("cave_in_bonus", -0.03),
    effect("advance_max_penalty", 1.0),
];
const FOSSIL_SEAM_EFFECTS: &[EffectSpec] = &[
    effect("artifact_multiplier", 1.75),
    effect("cave_in_bonus", 0.05),
    effect("luminosity_drain_multiplier", 0.25),
];
const STONE_ROUTES: [DigRoute; 3] = [
    DigRoute {
        id: "old_supports",
        name: "Old Supports",
        description: "Collapse risk falls 5 points and losses cap at 6 blocks, but maximum advance falls by 1.",
        layer: Some("Stone"),
        effects: OLD_SUPPORTS_EFFECTS,
    },
    DigRoute {
        id: "surveyors_cut",
        name: "Surveyor's Cut",
        description: "Events rise 25% and collapse risk falls 3 points, but maximum advance falls by 1.",
        layer: Some("Stone"),
        effects: SURVEYORS_CUT_EFFECTS,
    },
    DigRoute {
        id: "fossil_seam",
        name: "Fossil Seam",
        description: "Artifact odds rise 75%; collapses rise 5 points and light drains 25% faster.",
        layer: Some("Stone"),
        effects: FOSSIL_SEAM_EFFECTS,
    },
];

const PRISMATIC_FAULT_EFFECTS: &[EffectSpec] = &[
    effect("event_chance_multiplier", 0.50),
    effect("artifact_multiplier", 1.25),
    effect("cave_in_bonus", 0.06),
];
const GLASS_LABYRINTH_EFFECTS: &[EffectSpec] = &[
    effect("artifact_multiplier", 2.0),
    effect("advance_max_penalty", 1.0),
    effect("luminosity_drain_multiplier", 0.50),
];
const RESONANT_GALLERY_EFFECTS: &[EffectSpec] = &[
    effect("event_chance_multiplier", 0.75),
    effect("luminosity_drain_multiplier", 0.50),
    effect("cave_in_bonus", 0.04),
];
const CRYSTAL_ROUTES: [DigRoute; 3] = [
    DigRoute {
        id: "prismatic_fault",
        name: "Prismatic Fault",
        description: "Events rise 50% and artifact odds rise 25%, with 6 points more collapse risk.",
        layer: Some("Crystal"),
        effects: PRISMATIC_FAULT_EFFECTS,
    },
    DigRoute {
        id: "glass_labyrinth",
        name: "Glass Labyrinth",
        description: "Artifact odds double, but maximum advance falls by 1 and light drains 50% faster.",
        layer: Some("Crystal"),
        effects: GLASS_LABYRINTH_EFFECTS,
    },
    DigRoute {
        id: "resonant_gallery",
        name: "Resonant Gallery",
        description: "Events rise 75%; light drains 50% faster and collapses rise 4 points.",
        layer: Some("Crystal"),
        effects: RESONANT_GALLERY_EFFECTS,
    },
];

const COOLING_SLUICE_EFFECTS: &[EffectSpec] = &[
    effect("cave_in_bonus", -0.10),
    effect("luminosity_drain_reduction", 0.25),
    effect("advance_max_penalty", 1.0),
    effect("artifact_multiplier", 0.75),
];
const LAVA_TUBE_EFFECTS: &[EffectSpec] = &[
    effect("advance_bonus", 1.0),
    effect("cave_in_bonus", 0.07),
    effect("luminosity_drain_multiplier", 0.50),
];
const EMBER_VEIN_EFFECTS: &[EffectSpec] = &[
    effect("artifact_multiplier", 1.75),
    effect("event_chance_multiplier", 0.25),
    effect("luminosity_drain_multiplier", 0.75),
    effect("cave_in_bonus", 0.04),
];
const MAGMA_ROUTES: [DigRoute; 3] = [
    DigRoute {
        id: "cooling_sluice",
        name: "Cooling Sluice",
        description: "Collapse risk falls 10 points and light drain falls 25%, but advance and artifact odds suffer.",
        layer: Some("Magma"),
        effects: COOLING_SLUICE_EFFECTS,
    },
    DigRoute {
        id: "lava_tube",
        name: "Lava Tube",
        description: "+1 advance, with 7 points more collapse risk and 50% faster light drain.",
        layer: Some("Magma"),
        effects: LAVA_TUBE_EFFECTS,
    },
    DigRoute {
        id: "ember_vein",
        name: "Ember Vein",
        description: "Artifacts rise 75% and events 25%; light drains 75% faster and collapses rise 4 points.",
        layer: Some("Magma"),
        effects: EMBER_VEIN_EFFECTS,
    },
];

const WHISPERING_CUT_EFFECTS: &[EffectSpec] = &[
    effect("event_chance_multiplier", 0.75),
    effect("luminosity_drain_multiplier", 0.50),
    effect("cave_in_bonus", 0.05),
];
const BLIND_DESCENT_EFFECTS: &[EffectSpec] = &[
    effect("artifact_multiplier", 2.0),
    effect("luminosity_drain_multiplier", 1.0),
    effect("cave_in_bonus", 0.08),
];
const ANCHOR_LINE_EFFECTS: &[EffectSpec] = &[
    effect("cave_in_loss_cap", 7.0),
    effect("cave_in_bonus", -0.04),
    effect("advance_max_penalty", 1.0),
];
const ABYSS_ROUTES: [DigRoute; 3] = [
    DigRoute {
        id: "whispering_cut",
        name: "Whispering Cut",
        description: "Events rise 75%; light drains 50% faster and collapses rise 5 points.",
        layer: Some("Abyss"),
        effects: WHISPERING_CUT_EFFECTS,
    },
    DigRoute {
        id: "blind_descent",
        name: "Blind Descent",
        description: "Artifact odds double, but light drains twice as fast and collapses rise 8 points.",
        layer: Some("Abyss"),
        effects: BLIND_DESCENT_EFFECTS,
    },
    DigRoute {
        id: "anchor_line",
        name: "Anchor Line",
        description: "Collapse losses cap at 7 blocks and risk falls 4 points, but maximum advance falls by 1.",
        layer: Some("Abyss"),
        effects: ANCHOR_LINE_EFFECTS,
    },
];

const MYCELIAL_TRACK_EFFECTS: &[EffectSpec] = &[
    effect("advance_bonus", 1.0),
    effect("event_chance_multiplier", 0.25),
    effect("luminosity_drain_multiplier", 0.50),
    effect("cave_in_bonus", 0.04),
];
const SPORELIT_GARDEN_EFFECTS: &[EffectSpec] = &[
    effect("luminosity_drain_reduction", 0.50),
    effect("artifact_multiplier", 1.25),
    effect("event_chance_multiplier", -0.25),
    effect("cave_in_bonus", 0.05),
];
const ROTCAP_HOLLOW_EFFECTS: &[EffectSpec] = &[
    effect("event_chance_multiplier", 0.75),
    effect("artifact_multiplier", 1.50),
    effect("cave_in_bonus", 0.08),
    effect("luminosity_drain_multiplier", 0.25),
];
const FUNGAL_ROUTES: [DigRoute; 3] = [
    DigRoute {
        id: "mycelial_track",
        name: "Mycelial Track",
        description: "+1 advance and 25% more events; light drains 50% faster and collapses rise 4 points.",
        layer: Some("Fungal Depths"),
        effects: MYCELIAL_TRACK_EFFECTS,
    },
    DigRoute {
        id: "sporelit_garden",
        name: "Sporelit Garden",
        description: "Light drains 50% slower and artifacts rise 25%; events fall 25% and collapses rise 5 points.",
        layer: Some("Fungal Depths"),
        effects: SPORELIT_GARDEN_EFFECTS,
    },
    DigRoute {
        id: "rotcap_hollow",
        name: "Rotcap Hollow",
        description: "Events rise 75% and artifacts 50%; collapses rise 8 points and light drains 25% faster.",
        layer: Some("Fungal Depths"),
        effects: ROTCAP_HOLLOW_EFFECTS,
    },
];

const FROZEN_STILLNESS_EFFECTS: &[EffectSpec] = &[
    effect("cave_in_bonus", -0.12),
    effect("event_chance_multiplier", -0.50),
    effect("artifact_multiplier", 0.75),
    effect("advance_max_penalty", 1.0),
];
const TIMEFRACTURE_EFFECTS: &[EffectSpec] = &[
    effect("advance_bonus", 1.0),
    effect("event_chance_multiplier", 0.50),
    effect("cave_in_bonus", 0.08),
    effect("cave_in_loss_bonus", 1.0),
];
const ICEBOUND_CACHE_EFFECTS: &[EffectSpec] = &[
    effect("artifact_multiplier", 2.0),
    effect("advance_max_penalty", 1.0),
    effect("luminosity_drain_multiplier", 0.50),
    effect("cave_in_bonus", 0.04),
];
const FROZEN_ROUTES: [DigRoute; 3] = [
    DigRoute {
        id: "frozen_stillness",
        name: "Frozen Stillness",
        description: "Collapse risk falls 12 points, but events fall 50%, artifacts 25%, and maximum advance by 1.",
        layer: Some("Frozen Core"),
        effects: FROZEN_STILLNESS_EFFECTS,
    },
    DigRoute {
        id: "timefracture",
        name: "Timefracture",
        description: "+1 advance and 50% more events; collapses rise 8 points and cost 1 more block.",
        layer: Some("Frozen Core"),
        effects: TIMEFRACTURE_EFFECTS,
    },
    DigRoute {
        id: "icebound_cache",
        name: "Icebound Cache",
        description: "Artifact odds double, but maximum advance falls by 1, light drains 50% faster, and collapses rise 4 points.",
        layer: Some("Frozen Core"),
        effects: ICEBOUND_CACHE_EFFECTS,
    },
];

const VOID_HARVEST_EFFECTS: &[EffectSpec] = &[
    effect("artifact_multiplier", 2.0),
    effect("event_chance_multiplier", 0.50),
    effect("cave_in_bonus", 0.12),
    effect("luminosity_drain_multiplier", 0.50),
];
const SILENT_ROAD_EFFECTS: &[EffectSpec] = &[
    effect("cave_in_bonus", -0.10),
    effect("luminosity_drain_reduction", 0.50),
    effect("event_chance_multiplier", -0.50),
    effect("artifact_multiplier", 0.75),
    effect("advance_max_penalty", 1.0),
];
const MAWS_SHORTCUT_EFFECTS: &[EffectSpec] = &[
    effect("advance_bonus", 2.0),
    effect("cave_in_bonus", 0.15),
    effect("cave_in_loss_bonus", 3.0),
    effect("luminosity_drain_multiplier", 1.0),
];
const HOLLOW_ROUTES: [DigRoute; 3] = [
    DigRoute {
        id: "void_harvest",
        name: "Void Harvest",
        description: "Artifact odds double and events rise 50%; collapses rise 12 points and light drains 50% faster.",
        layer: Some("The Hollow"),
        effects: VOID_HARVEST_EFFECTS,
    },
    DigRoute {
        id: "silent_road",
        name: "Silent Road",
        description: "Collapse risk and light drain fall, but events, artifacts, and maximum advance all fall.",
        layer: Some("The Hollow"),
        effects: SILENT_ROAD_EFFECTS,
    },
    DigRoute {
        id: "maws_shortcut",
        name: "Maw's Shortcut",
        description: "+2 advance; collapses rise 15 points, cost 3 more blocks, and light drains twice as fast.",
        layer: Some("The Hollow"),
        effects: MAWS_SHORTCUT_EFFECTS,
    },
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerRoutePool {
    pub layer: &'static str,
    pub routes: &'static [DigRoute; 3],
}

pub const LAYER_ROUTE_POOLS: [LayerRoutePool; 7] = [
    LayerRoutePool {
        layer: "Stone",
        routes: &STONE_ROUTES,
    },
    LayerRoutePool {
        layer: "Crystal",
        routes: &CRYSTAL_ROUTES,
    },
    LayerRoutePool {
        layer: "Magma",
        routes: &MAGMA_ROUTES,
    },
    LayerRoutePool {
        layer: "Abyss",
        routes: &ABYSS_ROUTES,
    },
    LayerRoutePool {
        layer: "Fungal Depths",
        routes: &FUNGAL_ROUTES,
    },
    LayerRoutePool {
        layer: "Frozen Core",
        routes: &FROZEN_ROUTES,
    },
    LayerRoutePool {
        layer: "The Hollow",
        routes: &HOLLOW_ROUTES,
    },
];

pub const ROUTE_EFFECT_KEYS: [&str; 9] = [
    "advance_bonus",
    "advance_max_penalty",
    "artifact_multiplier",
    "cave_in_bonus",
    "cave_in_loss_bonus",
    "cave_in_loss_cap",
    "event_chance_multiplier",
    "luminosity_drain_multiplier",
    "luminosity_drain_reduction",
];

#[must_use]
pub fn route_by_id(route_id: &str) -> Option<&'static DigRoute> {
    UNIVERSAL_ROUTES
        .iter()
        .chain(LAYER_ROUTE_POOLS.iter().flat_map(|pool| pool.routes.iter()))
        .find(|route| route.id == route_id)
}

#[must_use]
pub fn route_catalog() -> Vec<&'static DigRoute> {
    UNIVERSAL_ROUTES
        .iter()
        .chain(LAYER_ROUTE_POOLS.iter().flat_map(|pool| pool.routes.iter()))
        .collect()
}

#[must_use]
pub fn route_effect(route: &DigRoute, key: &str) -> Option<f64> {
    route.effects.iter().find_map(|spec| {
        (spec.key == key)
            .then_some(spec.value)
            .and_then(EffectValue::number)
    })
}

/// Minimal entropy port used by offer generation.
pub trait RouteEntropy {
    fn index(&mut self, upper_exclusive: usize) -> usize;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteOfferError {
    pub layer: String,
}

impl Display for RouteOfferError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "No route pool for layer {:?}", self.layer)
    }
}

impl std::error::Error for RouteOfferError {}

pub fn generate_route_offer<R: RouteEntropy + ?Sized>(
    layer: &str,
    previous_route_id: Option<&str>,
    entropy: &mut R,
) -> Result<[&'static str; 3], RouteOfferError> {
    let themed_pool = LAYER_ROUTE_POOLS
        .iter()
        .find(|pool| pool.layer == layer)
        .ok_or_else(|| RouteOfferError {
            layer: layer.to_owned(),
        })?;
    let mut universal_ids: Vec<_> = UNIVERSAL_ROUTES
        .iter()
        .map(|route| route.id)
        .filter(|route_id| Some(*route_id) != previous_route_id)
        .collect();
    if universal_ids.is_empty() {
        universal_ids = UNIVERSAL_ROUTES.iter().map(|route| route.id).collect();
    }
    let universal = universal_ids[entropy.index(universal_ids.len()) % universal_ids.len()];

    let all_themed: Vec<_> = themed_pool.routes.iter().map(|route| route.id).collect();
    let without_previous: Vec<_> = all_themed
        .iter()
        .copied()
        .filter(|route_id| Some(*route_id) != previous_route_id)
        .collect();
    let mut themed_ids = if without_previous.len() >= 2 {
        without_previous
    } else {
        all_themed
    };
    let first_index = entropy.index(themed_ids.len()) % themed_ids.len();
    let first = themed_ids.remove(first_index);
    let second_index = entropy.index(themed_ids.len()) % themed_ids.len();
    let second = themed_ids[second_index];
    let mut offered = [universal, first, second];
    for end in (1..offered.len()).rev() {
        let swap_with = entropy.index(end + 1) % (end + 1);
        offered.swap(end, swap_with);
    }
    Ok(offered)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteState {
    pub layer: String,
    pub start_depth: i64,
    pub end_depth: i64,
    pub offered: [String; 3],
    pub selected: Option<String>,
}

impl RouteState {
    #[must_use]
    pub fn to_python_json(&self) -> String {
        let selected = self
            .selected
            .as_deref()
            .map_or_else(|| "null".to_owned(), json_string);
        format!(
            "{{\"end_depth\": {}, \"layer\": {}, \"offered\": [{}, {}, {}], \"selected\": {}, \"start_depth\": {}}}",
            self.end_depth,
            json_string(&self.layer),
            json_string(&self.offered[0]),
            json_string(&self.offered[1]),
            json_string(&self.offered[2]),
            selected,
            self.start_depth
        )
    }
}

#[must_use]
pub fn parse_route_state(raw: Option<&str>) -> Option<RouteState> {
    let raw = raw?.trim();
    if raw.is_empty() || !raw.starts_with('{') || !raw.ends_with('}') {
        return None;
    }
    let layer = string_field(raw, "layer")?;
    let start_depth = integer_field(raw, "start_depth")?;
    let end_depth = integer_field(raw, "end_depth")?;
    let offered_values = string_array_field(raw, "offered")?;
    let offered: [String; 3] = offered_values.try_into().ok()?;
    if offered
        .iter()
        .any(|route_id| route_by_id(route_id).is_none())
    {
        return None;
    }
    let selected = nullable_string_field(raw, "selected")?;
    if selected
        .as_ref()
        .is_some_and(|selected| !offered.contains(selected))
    {
        return None;
    }
    Some(RouteState {
        layer,
        start_depth,
        end_depth,
        offered,
        selected,
    })
}

pub fn build_route_offer_state<R: RouteEntropy + ?Sized>(
    previous_raw_state: Option<&str>,
    defeated_boundary: i64,
    entropy: &mut R,
) -> Result<Option<RouteState>, RouteOfferError> {
    let Some(boundary_index) = BOSS_BOUNDARIES
        .iter()
        .position(|boundary| *boundary == defeated_boundary)
    else {
        return Ok(None);
    };
    let next_layer = LAYERS[boundary_index + 1].name;
    let end_depth = BOSS_BOUNDARIES
        .get(boundary_index + 1)
        .copied()
        .unwrap_or(PINNACLE_DEPTH);
    let previous_universal = parse_route_state(previous_raw_state).and_then(|state| {
        state
            .offered
            .into_iter()
            .find(|route_id| route_by_id(route_id).is_some_and(|route| route.layer.is_none()))
    });
    let offered = generate_route_offer(next_layer, previous_universal.as_deref(), entropy)?;
    Ok(Some(RouteState {
        layer: next_layer.to_owned(),
        start_depth: defeated_boundary,
        end_depth,
        offered: offered.map(str::to_owned),
        selected: None,
    }))
}

#[derive(Clone, Debug, PartialEq)]
pub struct RouteStatus {
    pub choice_required: bool,
    pub layer: Option<String>,
    pub start_depth: Option<i64>,
    pub end_depth: Option<i64>,
    pub offered_routes: Vec<&'static DigRoute>,
    pub active_route: Option<&'static DigRoute>,
}

impl RouteStatus {
    #[must_use]
    pub fn none() -> Self {
        Self {
            choice_required: false,
            layer: None,
            start_depth: None,
            end_depth: None,
            offered_routes: Vec::new(),
            active_route: None,
        }
    }
}

#[must_use]
pub fn route_status(raw_state: Option<&str>) -> RouteStatus {
    let Some(state) = parse_route_state(raw_state) else {
        return RouteStatus::none();
    };
    let offered_routes = state
        .offered
        .iter()
        .filter_map(|route_id| route_by_id(route_id))
        .collect();
    let active_route = state.selected.as_deref().and_then(route_by_id);
    RouteStatus {
        choice_required: state.selected.is_none(),
        layer: Some(state.layer),
        start_depth: Some(state.start_depth),
        end_depth: Some(state.end_depth),
        offered_routes,
        active_route,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RouteChoiceResult {
    pub success: bool,
    pub error: Option<String>,
    pub route: Option<&'static DigRoute>,
    pub already_selected: bool,
}

impl RouteChoiceResult {
    fn error(message: &str) -> Self {
        Self {
            success: false,
            error: Some(message.to_owned()),
            route: None,
            already_selected: false,
        }
    }

    fn selected(route: &'static DigRoute, already_selected: bool) -> Self {
        Self {
            success: true,
            error: None,
            route: Some(route),
            already_selected,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RouteChoiceEvaluation {
    Select {
        route: &'static DigRoute,
        selected_state: RouteState,
    },
    AlreadySelected {
        route: &'static DigRoute,
    },
    Rejected(&'static str),
}

#[must_use]
pub fn evaluate_route_choice(raw_state: Option<&str>, route_id: &str) -> RouteChoiceEvaluation {
    let Some(mut state) = parse_route_state(raw_state) else {
        return RouteChoiceEvaluation::Rejected("There is no route choice waiting for you.");
    };
    if let Some(selected) = state.selected.as_deref() {
        return if selected == route_id {
            RouteChoiceEvaluation::AlreadySelected {
                route: route_by_id(selected).expect("validated persisted route"),
            }
        } else {
            RouteChoiceEvaluation::Rejected("Your route for this layer is already locked in.")
        };
    }
    if !state.offered.iter().any(|offered| offered == route_id) {
        return RouteChoiceEvaluation::Rejected("That route was not offered for this layer.");
    }
    let route = route_by_id(route_id).expect("offered routes were validated");
    state.selected = Some(route_id.to_owned());
    RouteChoiceEvaluation::Select {
        route,
        selected_state: state,
    }
}

#[derive(Debug)]
pub enum DigRouteAppError {
    Repository(DigRouteRepositoryError),
}

impl Display for DigRouteAppError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(error) => Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for DigRouteAppError {}

impl From<DigRouteRepositoryError> for DigRouteAppError {
    fn from(error: DigRouteRepositoryError) -> Self {
        Self::Repository(error)
    }
}

#[derive(Clone, Debug)]
pub struct DigRouteService {
    repository: DigRouteRepository,
}

impl DigRouteService {
    #[must_use]
    pub const fn new(repository: DigRouteRepository) -> Self {
        Self { repository }
    }

    pub fn get_route_status(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<RouteStatus>, DigRouteAppError> {
        Ok(self
            .repository
            .tunnel(discord_id, guild_id)?
            .map(|tunnel| route_status(tunnel.route_state.as_deref())))
    }

    pub fn choose_route(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        route_id: &str,
        now: i64,
    ) -> Result<RouteChoiceResult, DigRouteAppError> {
        let Some(tunnel) = self.repository.tunnel(discord_id, guild_id)? else {
            return Ok(RouteChoiceResult::error("You don't have a tunnel."));
        };
        let raw_pending = tunnel.route_state.as_deref();
        let (route, state) = match evaluate_route_choice(raw_pending, route_id) {
            RouteChoiceEvaluation::Select {
                route,
                selected_state,
            } => (route, selected_state),
            RouteChoiceEvaluation::AlreadySelected { route } => {
                return Ok(RouteChoiceResult::selected(route, true));
            }
            RouteChoiceEvaluation::Rejected(message) => {
                return Ok(RouteChoiceResult::error(message));
            }
        };
        let raw_pending = raw_pending.expect("select evaluation requires persisted route state");
        let selected_json = state.to_python_json();
        let detail_json = format!(
            "{{\"route_id\": {}, \"layer\": {}}}",
            json_string(route_id),
            json_string(&state.layer)
        );
        match self.repository.choose_route_atomic(RouteChoiceRequest {
            discord_id,
            guild_id,
            expected_route_state: raw_pending,
            selected_route_state: &selected_json,
            route_id,
            layer: &state.layer,
            detail_json: &detail_json,
            now,
        })? {
            RouteChoiceClaim::Claimed => Ok(RouteChoiceResult::selected(route, false)),
            RouteChoiceClaim::MissingTunnel => {
                Ok(RouteChoiceResult::error("You don't have a tunnel."))
            }
            RouteChoiceClaim::Conflict {
                persisted_route_state,
            } => {
                let persisted = parse_route_state(persisted_route_state.as_deref());
                Ok(
                    if persisted
                        .as_ref()
                        .and_then(|state| state.selected.as_deref())
                        == Some(route_id)
                    {
                        RouteChoiceResult::selected(route, true)
                    } else {
                        RouteChoiceResult::error(
                            "Your route choice changed before it could be saved.",
                        )
                    },
                )
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingRouteBlock {
    pub success: bool,
    pub error: &'static str,
    pub route_choice_required: bool,
    pub status: RouteStatus,
}

#[must_use]
pub fn pending_route_block(raw_state: Option<&str>) -> Option<PendingRouteBlock> {
    let status = route_status(raw_state);
    status.choice_required.then_some(PendingRouteBlock {
        success: false,
        error: "Choose a route before continuing your expedition.",
        route_choice_required: true,
        status,
    })
}

pub fn live_dig_after_route_gate<T>(
    raw_state: Option<&str>,
    after_gate: impl FnOnce() -> T,
) -> Result<T, PendingRouteBlock> {
    if let Some(blocked) = pending_route_block(raw_state) {
        return Err(blocked);
    }
    Ok(after_gate())
}

pub fn deterministic_preconditions_after_route_gate<T>(
    raw_state: Option<&str>,
    after_gate: impl FnOnce() -> T,
) -> Result<T, PendingRouteBlock> {
    if let Some(blocked) = pending_route_block(raw_state) {
        return Err(blocked);
    }
    Ok(after_gate())
}

#[derive(Clone, Debug, PartialEq)]
pub struct BossConflictResult {
    pub success: bool,
    pub error: &'static str,
    pub route_choice: Option<RouteStatus>,
    pub route_choice_required: bool,
}

#[must_use]
pub fn boss_resolution_conflict_result(tunnel: Option<&RouteTunnelRecord>) -> BossConflictResult {
    let route_choice = tunnel.map(|tunnel| route_status(tunnel.route_state.as_deref()));
    let route_choice_required = route_choice
        .as_ref()
        .is_some_and(|status| status.choice_required);
    BossConflictResult {
        success: false,
        error: if route_choice_required {
            "This boss fight was already resolved. The saved route remains available."
        } else {
            "This boss fight was already resolved by another view."
        },
        route_choice,
        route_choice_required,
    }
}

pub fn after_successful_boss_claim<T>(
    claim: &BossOutcomeClaim,
    side_effect: impl FnOnce() -> T,
) -> Result<T, BossConflictResult> {
    match claim {
        BossOutcomeClaim::Claimed => Ok(side_effect()),
        BossOutcomeClaim::Conflict { persisted } => {
            Err(boss_resolution_conflict_result(Some(persisted)))
        }
        BossOutcomeClaim::MissingTunnel | BossOutcomeClaim::MissingPlayer => {
            Err(boss_resolution_conflict_result(None))
        }
    }
}

pub fn clear_route_for_abandon(tunnel: &mut Tunnel) {
    tunnel.route_state = None;
}

pub fn clear_route_for_prestige(tunnel: &mut Tunnel) {
    tunnel.route_state = None;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoutePreconditionInput {
    pub layer: Layer,
    pub luminosity: i64,
    pub prestige_cave_in_multiplier: f64,
    pub base_event_chance: f64,
    pub thick_skin_saved: bool,
    pub social_cave_in_bonus: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoutePreconditions {
    pub cave_in_chance: f64,
    pub advance_min: i64,
    pub advance_max: i64,
    pub luminosity: i64,
    pub event_chance: f64,
    pub artifact_multiplier: f64,
    pub thick_skin_saved: bool,
}

#[must_use]
pub fn apply_route_preconditions(
    input: RoutePreconditionInput,
    route: Option<&DigRoute>,
) -> RoutePreconditions {
    let value = |key: &str, default: f64| {
        route
            .and_then(|route| route_effect(route, key))
            .unwrap_or(default)
    };
    let cave_in_bonus = value("cave_in_bonus", 0.0);
    let advance_bonus = value("advance_bonus", 0.0) as i64;
    let advance_max_penalty = value("advance_max_penalty", 0.0) as i64;
    let drain_factor = (1.0 + value("luminosity_drain_multiplier", 0.0)
        - value("luminosity_drain_reduction", 0.0))
    .max(0.0);
    let drained = (input.layer.luminosity_drain as f64 * drain_factor) as i64;
    let advance_min = input.layer.advance_range.0 + advance_bonus;
    let advance_max = (input.layer.advance_range.1 - advance_max_penalty)
        .max(input.layer.advance_range.0)
        + advance_bonus;
    let mut cave_in_chance = ((input.layer.cave_in_chance + cave_in_bonus)
        * input.prestige_cave_in_multiplier)
        .max(0.01);
    cave_in_chance += input.social_cave_in_bonus;
    if input.thick_skin_saved {
        cave_in_chance = 0.0;
    } else {
        cave_in_chance = cave_in_chance.max(0.01);
    }
    RoutePreconditions {
        cave_in_chance,
        advance_min,
        advance_max,
        luminosity: (input.luminosity - drained).max(0),
        event_chance: (input.base_event_chance * (1.0 + value("event_chance_multiplier", 0.0)))
            .min(0.75),
        artifact_multiplier: value("artifact_multiplier", 1.0),
        thick_skin_saved: input.thick_skin_saved,
    }
}

#[must_use]
pub fn live_route_preconditions(
    input: RoutePreconditionInput,
    route: Option<&DigRoute>,
) -> RoutePreconditions {
    apply_route_preconditions(input, route)
}

#[must_use]
pub fn deterministic_route_preconditions(
    input: RoutePreconditionInput,
    route: Option<&DigRoute>,
) -> RoutePreconditions {
    apply_route_preconditions(input, route)
}

#[must_use]
pub fn effective_cave_in_loss_cap(
    route: Option<&DigRoute>,
    additional_caps: &[Option<i64>],
) -> Option<i64> {
    let route_cap = route.and_then(|route| route_effect(route, "cave_in_loss_cap"));
    route_cap
        .map(|cap| cap as i64)
        .into_iter()
        .chain(additional_caps.iter().copied().flatten())
        .min()
}

#[must_use]
pub fn apply_route_cave_in_loss(
    block_loss: i64,
    route: Option<&DigRoute>,
    additional_caps: &[Option<i64>],
) -> i64 {
    let bonus = route
        .and_then(|route| route_effect(route, "cave_in_loss_bonus"))
        .unwrap_or(0.0) as i64;
    apply_cave_in_loss_effects(
        block_loss,
        bonus,
        route
            .and_then(|route| route_effect(route, "cave_in_loss_cap"))
            .map(|cap| cap as i64),
        additional_caps,
    )
}

#[must_use]
pub fn apply_cave_in_loss_effects(
    block_loss: i64,
    loss_bonus: i64,
    route_cap: Option<i64>,
    additional_caps: &[Option<i64>],
) -> i64 {
    let loss = block_loss.saturating_add(loss_bonus);
    route_cap
        .into_iter()
        .chain(additional_caps.iter().copied().flatten())
        .min()
        .map_or(loss, |cap| loss.min(cap))
        .max(0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RouteCaveInInput {
    pub depth_before: i64,
    pub rolled_loss: i64,
    pub weather_loss_cap: Option<i64>,
    pub steady_hands_reduction: f64,
    pub reinforcement_active: bool,
    pub catastrophic: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteCaveInOutcome {
    pub block_loss: i64,
    pub depth_after: i64,
    pub catastrophic: bool,
}

#[must_use]
pub fn resolve_route_cave_in(
    input: RouteCaveInInput,
    route: Option<&DigRoute>,
) -> RouteCaveInOutcome {
    let mut loss = apply_route_cave_in_loss(input.rolled_loss, route, &[input.weather_loss_cap]);
    if input.steady_hands_reduction > 0.0 {
        loss = (loss as f64 * (1.0 - input.steady_hands_reduction)) as i64;
    }
    let reinforcement_cap = input.reinforcement_active.then_some(8);
    if let Some(cap) = reinforcement_cap {
        loss = loss.min(cap);
    }
    let depth_after = if input.catastrophic {
        let milestone = ((input.depth_before - 1).max(0) / 25) * 25;
        effective_cave_in_loss_cap(route, &[input.weather_loss_cap, reinforcement_cap])
            .map_or(milestone, |cap| {
                milestone.max(input.depth_before.saturating_sub(cap.max(0)))
            })
    } else {
        input.depth_before.saturating_sub(loss).max(0)
    };
    RouteCaveInOutcome {
        block_loss: input.depth_before.saturating_sub(depth_after),
        depth_after,
        catastrophic: input.catastrophic,
    }
}

#[must_use]
pub fn applied_outcome_route_cave_in_loss(
    requested_loss: i64,
    route: Option<&DigRoute>,
    weather_loss_cap: Option<i64>,
    reinforcement_active: bool,
) -> i64 {
    let mut loss = apply_route_cave_in_loss(requested_loss, route, &[]);
    if let Some(cap) = weather_loss_cap {
        loss = loss.min(cap);
    }
    if reinforcement_active {
        loss = loss.min(8);
    }
    loss
}

#[must_use]
pub fn route_artifact_multiplier(route: Option<&DigRoute>) -> f64 {
    route
        .and_then(|route| route_effect(route, "artifact_multiplier"))
        .unwrap_or(1.0)
}

fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                write!(escaped, "\\u{:04x}", character as u32)
                    .expect("writing to String cannot fail");
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

fn field_value_start(raw: &str, key: &str) -> Option<usize> {
    let needle = format!("\"{key}\"");
    let mut search_from = 0;
    while let Some(relative) = raw[search_from..].find(&needle) {
        let after_key = search_from + relative + needle.len();
        let tail = &raw[after_key..];
        let colon = tail.find(':')?;
        if tail[..colon].chars().all(char::is_whitespace) {
            let after_colon = after_key + colon + 1;
            return Some(
                after_colon
                    + raw[after_colon..].find(|character: char| !character.is_whitespace())?,
            );
        }
        search_from = after_key;
    }
    None
}

fn parse_json_string(raw: &str, start: usize) -> Option<(String, usize)> {
    let bytes = raw.as_bytes();
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut output = String::new();
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return Some((output, index + 1)),
            b'\\' => {
                index += 1;
                match *bytes.get(index)? {
                    b'"' => output.push('"'),
                    b'\\' => output.push('\\'),
                    b'/' => output.push('/'),
                    b'b' => output.push('\u{0008}'),
                    b'f' => output.push('\u{000c}'),
                    b'n' => output.push('\n'),
                    b'r' => output.push('\r'),
                    b't' => output.push('\t'),
                    _ => return None,
                }
                index += 1;
            }
            byte if byte.is_ascii_control() => return None,
            _ => {
                let character = raw[index..].chars().next()?;
                output.push(character);
                index += character.len_utf8();
            }
        }
    }
    None
}

fn string_field(raw: &str, key: &str) -> Option<String> {
    parse_json_string(raw, field_value_start(raw, key)?).map(|(value, _)| value)
}

fn nullable_string_field(raw: &str, key: &str) -> Option<Option<String>> {
    let start = field_value_start(raw, key)?;
    if raw[start..].starts_with("null") {
        Some(None)
    } else {
        parse_json_string(raw, start).map(|(value, _)| Some(value))
    }
}

fn integer_field(raw: &str, key: &str) -> Option<i64> {
    let start = field_value_start(raw, key)?;
    let length = raw[start..]
        .find(|character: char| !character.is_ascii_digit() && character != '-')
        .unwrap_or(raw.len() - start);
    raw[start..start + length].parse().ok()
}

fn string_array_field(raw: &str, key: &str) -> Option<Vec<String>> {
    let mut index = field_value_start(raw, key)?;
    if raw.as_bytes().get(index) != Some(&b'[') {
        return None;
    }
    index += 1;
    let mut values = Vec::new();
    loop {
        while raw
            .as_bytes()
            .get(index)
            .is_some_and(u8::is_ascii_whitespace)
        {
            index += 1;
        }
        if raw.as_bytes().get(index) == Some(&b']') {
            return Some(values);
        }
        let (value, end) = parse_json_string(raw, index)?;
        values.push(value);
        index = end;
        while raw
            .as_bytes()
            .get(index)
            .is_some_and(u8::is_ascii_whitespace)
        {
            index += 1;
        }
        match raw.as_bytes().get(index) {
            Some(b',') => index += 1,
            Some(b']') => return Some(values),
            _ => return None,
        }
    }
}

#[cfg(test)]
#[path = "dig_routes/tests.rs"]
mod tests;
