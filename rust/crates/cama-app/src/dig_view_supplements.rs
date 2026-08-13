//! Smaller Dig route, event-view, and phase-transition policies.
//!
//! The live Python commands remain authoritative. These typed state machines
//! keep Discord acknowledgement/removal behavior and one-shot economic guards
//! executable while the Rust transport and repository adapters are built.

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteOffer {
    pub id: String,
    pub name: String,
    pub description: String,
    pub layer: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteChoice {
    pub layer: String,
    pub start_depth: i64,
    pub end_depth: i64,
    pub offered_routes: Vec<RouteOffer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteField {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RouteEmbed {
    pub title: String,
    pub description: String,
    pub color: u32,
    pub fields: Vec<RouteField>,
    pub footer: String,
}

#[must_use]
pub const fn layer_color(layer: &str) -> u32 {
    match layer.as_bytes() {
        b"Dirt" => 0x8b_45_13,
        b"Stone" => 0x80_80_80,
        b"Crystal" => 0x00_ce_d1,
        b"Magma" => 0xff_45_00,
        b"Abyss" => 0x2f_00_47,
        b"Fungal Depths" => 0x7c_fc_00,
        b"Frozen Core" => 0x87_ce_eb,
        b"The Hollow" => 0x0d_0d_0d,
        _ => 0x8b_45_13,
    }
}

#[must_use]
pub fn build_route_choice_embed(choice: &RouteChoice) -> RouteEmbed {
    RouteEmbed {
        title: format!("{} Junction", choice.layer),
        description: format!(
            "The way forward splits after depth **{}**. Choose the passage that will shape your descent toward **{}**.\n\nThis choice remains active until another junction replaces it.",
            choice.start_depth, choice.end_depth
        ),
        color: layer_color(&choice.layer),
        fields: choice
            .offered_routes
            .iter()
            .map(|route| RouteField {
                name: route.name.clone(),
                value: route.description.clone(),
            })
            .collect(),
        footer: "No rerolls. If this view expires, use /dig go to reopen it.".to_owned(),
    }
}

pub fn add_route_choice_fields(embed: &mut RouteEmbed, choice: &RouteChoice) {
    embed.title = format!("{} Junction", choice.layer);
    embed.color = layer_color(&choice.layer);
    embed.fields.push(RouteField {
        name: format!("The path splits toward {}", choice.layer),
        value: "Choose one route below. It remains active until another junction replaces it."
            .to_owned(),
    });
    embed
        .fields
        .extend(choice.offered_routes.iter().map(|route| RouteField {
            name: route.name.clone(),
            value: route.description.clone(),
        }));
}

#[must_use]
pub fn build_locked_route_embed(route: &RouteOffer, layer: &str) -> RouteEmbed {
    RouteEmbed {
        title: format!("Route Locked: {}", route.name),
        description: format!(
            "**{}** will guide you through **{}**.\n\n{}",
            route.name, layer, route.description
        ),
        color: layer_color(layer),
        ..RouteEmbed::default()
    }
}

pub trait RouteSelectionPort {
    fn choose_route(
        &mut self,
        user_id: i64,
        guild_id: Option<i64>,
        route_id: &str,
    ) -> Result<RouteOffer, String>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteViewOutcome {
    Unauthorized,
    Duplicate,
    TimedOut,
    Failed(String),
    Locked {
        embed: RouteEmbed,
        remove_view: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteChoiceView {
    owner_id: i64,
    guild_id: Option<i64>,
    choice: RouteChoice,
    resolved: bool,
    disabled: Vec<bool>,
    edit_count: usize,
}

impl RouteChoiceView {
    #[must_use]
    pub fn new(owner_id: i64, guild_id: Option<i64>, choice: RouteChoice) -> Self {
        let disabled = vec![false; choice.offered_routes.len()];
        Self {
            owner_id,
            guild_id,
            choice,
            resolved: false,
            disabled,
            edit_count: 0,
        }
    }

    #[must_use]
    pub fn disabled(&self) -> &[bool] {
        &self.disabled
    }

    #[must_use]
    pub const fn is_resolved(&self) -> bool {
        self.resolved
    }

    #[must_use]
    pub const fn edit_count(&self) -> usize {
        self.edit_count
    }

    pub fn on_timeout(&mut self) -> RouteViewOutcome {
        self.disabled.fill(true);
        self.edit_count += 1;
        RouteViewOutcome::TimedOut
    }

    pub fn click(
        &mut self,
        actor_id: i64,
        route_index: usize,
        port: &mut dyn RouteSelectionPort,
    ) -> RouteViewOutcome {
        if actor_id != self.owner_id {
            return RouteViewOutcome::Unauthorized;
        }
        if self.resolved {
            return RouteViewOutcome::Duplicate;
        }
        let Some(offered) = self.choice.offered_routes.get(route_index) else {
            return RouteViewOutcome::Failed("Route selection failed.".to_owned());
        };
        self.resolved = true;
        match port.choose_route(self.owner_id, self.guild_id, &offered.id) {
            Ok(route) => {
                self.disabled.fill(true);
                self.edit_count += 1;
                RouteViewOutcome::Locked {
                    embed: build_locked_route_embed(&route, &self.choice.layer),
                    remove_view: true,
                }
            }
            Err(error) => {
                self.resolved = false;
                RouteViewOutcome::Failed(error)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigDispatchPlan {
    RouteChoiceRecovery,
    Continue,
}

#[must_use]
pub const fn dispatch_dig_result(route_choice_required: bool) -> DigDispatchPlan {
    if route_choice_required {
        DigDispatchPlan::RouteChoiceRecovery
    } else {
        DigDispatchPlan::Continue
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncounterClickOutcome {
    Resolved,
    Duplicate,
    Unauthorized,
}

/// Shared one-shot fence for event and boon component callbacks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncounterViewGuard {
    owner_id: i64,
    resolved: bool,
    controls_disabled: bool,
    edit_count: usize,
}

impl EncounterViewGuard {
    #[must_use]
    pub const fn new(owner_id: i64) -> Self {
        Self {
            owner_id,
            resolved: false,
            controls_disabled: false,
            edit_count: 0,
        }
    }

    pub fn click(
        &mut self,
        actor_id: i64,
        choice: &str,
        mut resolve: impl FnMut(&str),
    ) -> EncounterClickOutcome {
        if actor_id != self.owner_id {
            return EncounterClickOutcome::Unauthorized;
        }
        if self.resolved {
            return EncounterClickOutcome::Duplicate;
        }
        // Set the fence before service resolution; the Python callback awaits
        // immediately after this point and a second component can race it.
        self.resolved = true;
        self.controls_disabled = true;
        self.edit_count += 1;
        resolve(choice);
        EncounterClickOutcome::Resolved
    }

    #[must_use]
    pub const fn is_resolved(&self) -> bool {
        self.resolved
    }

    #[must_use]
    pub const fn controls_disabled(&self) -> bool {
        self.controls_disabled
    }

    #[must_use]
    pub const fn edit_count(&self) -> usize {
        self.edit_count
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PinnaclePhaseState {
    pub pending_phase_event_id: Option<String>,
}

#[must_use]
pub const fn phase_event_boss_hp_delta(event_id: &str) -> i32 {
    match event_id.as_bytes() {
        b"cave_in" => -1,
        b"void_pull" => -2,
        _ => 0,
    }
}

impl PinnaclePhaseState {
    /// Consume a phase transition exactly once when the next fight starts.
    pub fn starting_boss_hp(&mut self, fresh_hp: i32) -> i32 {
        let delta = self
            .pending_phase_event_id
            .take()
            .as_deref()
            .map_or(0, phase_event_boss_hp_delta);
        fresh_hp.saturating_add(delta).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route_choice() -> RouteChoice {
        RouteChoice {
            layer: "Stone".to_owned(),
            start_depth: 25,
            end_depth: 50,
            offered_routes: [
                (
                    "shored_passage",
                    "Shored Passage",
                    "Safer, but slower.",
                    None,
                ),
                (
                    "old_supports",
                    "Old Supports",
                    "Old braces hold the stone back.",
                    Some("Stone"),
                ),
                (
                    "fossil_seam",
                    "Fossil Seam",
                    "A rich but unstable seam.",
                    Some("Stone"),
                ),
            ]
            .into_iter()
            .map(|(id, name, description, layer)| RouteOffer {
                id: id.to_owned(),
                name: name.to_owned(),
                description: description.to_owned(),
                layer: layer.map(str::to_owned),
            })
            .collect(),
        }
    }

    #[derive(Default)]
    struct FakeRoutePort {
        calls: Vec<(i64, Option<i64>, String)>,
    }

    impl RouteSelectionPort for FakeRoutePort {
        fn choose_route(
            &mut self,
            user_id: i64,
            guild_id: Option<i64>,
            route_id: &str,
        ) -> Result<RouteOffer, String> {
            self.calls.push((user_id, guild_id, route_id.to_owned()));
            route_choice()
                .offered_routes
                .into_iter()
                .find(|route| route.id == route_id)
                .ok_or_else(|| "unknown route".to_owned())
        }
    }

    #[test]
    fn test_route_choice_embed_and_boss_fields_show_the_persisted_offer() {
        let choice = route_choice();
        let embed = build_route_choice_embed(&choice);
        assert_eq!(embed.title, "Stone Junction");
        assert!(embed.description.contains("25") && embed.description.contains("50"));
        assert_eq!(
            embed
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["Shored Passage", "Old Supports", "Fossil Seam"]
        );
        assert!(!embed.footer.contains("next guardian falls"));

        let mut boss_embed = RouteEmbed {
            title: "Boss Fight Result".to_owned(),
            color: 0x00_ff_00,
            ..RouteEmbed::default()
        };
        add_route_choice_fields(&mut boss_embed, &choice);
        assert_eq!(boss_embed.title, "Stone Junction");
        assert_eq!(boss_embed.color, layer_color("Stone"));
        assert_eq!(boss_embed.fields.len(), 4);
        assert!(!boss_embed.fields[0].value.contains("next guardian falls"));
    }

    #[test]
    fn test_route_view_timeout_disables_buttons_without_auto_selecting() {
        let mut view = RouteChoiceView::new(10_001, Some(12_345), route_choice());
        assert_eq!(view.on_timeout(), RouteViewOutcome::TimedOut);
        assert_eq!(view.disabled(), [true, true, true]);
        assert!(!view.is_resolved());
        assert_eq!(view.edit_count(), 1);
    }

    #[test]
    fn test_route_button_locks_selection_and_replaces_junction_embed() {
        let mut port = FakeRoutePort::default();
        let mut view = RouteChoiceView::new(10_001, Some(12_345), route_choice());
        let outcome = view.click(10_001, 1, &mut port);
        assert_eq!(
            port.calls,
            [(10_001, Some(12_345), "old_supports".to_owned())]
        );
        let RouteViewOutcome::Locked { embed, remove_view } = outcome else {
            panic!("route should lock");
        };
        assert_eq!(embed.title, "Route Locked: Old Supports");
        assert!(remove_view);
    }

    #[test]
    fn test_dispatch_routes_pending_choice_to_recovery_view() {
        assert_eq!(
            dispatch_dig_result(true),
            DigDispatchPlan::RouteChoiceRecovery
        );
    }

    #[test]
    fn test_event_encounter_ignores_double_click() {
        let mut view = EncounterViewGuard::new(42);
        let mut calls = Vec::new();
        assert_eq!(
            view.click(42, "safe", |choice| calls.push(choice.to_owned())),
            EncounterClickOutcome::Resolved
        );
        assert_eq!(
            view.click(42, "risky", |choice| calls.push(choice.to_owned())),
            EncounterClickOutcome::Duplicate
        );
        assert_eq!(calls, ["safe"]);
        assert!(view.is_resolved());
        assert!(view.controls_disabled());
        assert_eq!(view.edit_count(), 1);
    }

    #[test]
    fn test_boon_selection_ignores_double_click_and_locks_controls() {
        let mut view = EncounterViewGuard::new(42);
        let mut calls = Vec::new();
        assert_eq!(
            view.click(42, "boon_probe:boon_0", |choice| calls
                .push(choice.to_owned())),
            EncounterClickOutcome::Resolved
        );
        assert_eq!(
            view.click(42, "boon_probe:boon_0", |choice| calls
                .push(choice.to_owned())),
            EncounterClickOutcome::Duplicate
        );
        assert_eq!(calls, ["boon_probe:boon_0"]);
        assert!(view.controls_disabled());
        assert_eq!(view.edit_count(), 1);
    }

    #[test]
    fn test_event_encounter_rejects_non_owner() {
        let mut view = EncounterViewGuard::new(42);
        let mut calls = Vec::new();
        assert_eq!(
            view.click(999, "safe", |choice| calls.push(choice.to_owned())),
            EncounterClickOutcome::Unauthorized
        );
        assert!(calls.is_empty());
        assert!(!view.is_resolved());
    }

    #[test]
    fn test_negative_boss_hp_delta_wounds_next_phase() {
        let mut with_event = PinnaclePhaseState {
            pending_phase_event_id: Some("void_pull".to_owned()),
        };
        let mut control = PinnaclePhaseState::default();
        assert_eq!(
            with_event.starting_boss_hp(30),
            control.starting_boss_hp(30) - 2
        );
        assert!(with_event.pending_phase_event_id.is_none());
    }

    #[test]
    fn test_zero_boss_hp_delta_leaves_next_phase_unchanged() {
        let mut with_event = PinnaclePhaseState {
            pending_phase_event_id: Some("quiet".to_owned()),
        };
        let mut control = PinnaclePhaseState::default();
        assert_eq!(
            with_event.starting_boss_hp(30),
            control.starting_boss_hp(30)
        );
        assert!(with_event.pending_phase_event_id.is_none());
    }

    #[test]
    fn route_service_failure_releases_the_selection_fence_for_retry() {
        struct FailingPort;
        impl RouteSelectionPort for FailingPort {
            fn choose_route(
                &mut self,
                _user_id: i64,
                _guild_id: Option<i64>,
                _route_id: &str,
            ) -> Result<RouteOffer, String> {
                Err("passage shifted".to_owned())
            }
        }
        let mut view = RouteChoiceView::new(10_001, Some(12_345), route_choice());
        assert_eq!(
            view.click(10_001, 0, &mut FailingPort),
            RouteViewOutcome::Failed("passage shifted".to_owned())
        );
        assert!(!view.is_resolved());
    }
}
