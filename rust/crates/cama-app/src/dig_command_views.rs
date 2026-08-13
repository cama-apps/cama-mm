//! Discord-independent policies for the smaller Dig command views.
//!
//! Python remains the live Discord adapter.  These models preserve callback
//! ordering, pagination, private replies, and one-shot guards without needing
//! a gateway connection.

use cama_domain::embed_safety::EmbedModel;

pub const DISCORD_SELECT_OPTION_LIMIT: usize = 25;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RespecCommandResult {
    pub success: bool,
    pub cost: i64,
    pub returned_points: i64,
    pub total_unspent_points: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivateEmbedReply {
    pub embed: EmbedModel,
    pub ephemeral: bool,
}

#[must_use]
pub fn render_respec_result(result: &RespecCommandResult) -> Option<PrivateEmbedReply> {
    result.success.then(|| PrivateEmbedReply {
        embed: EmbedModel {
            title: Some("S Points Reset".to_owned()),
            description: Some(format!(
                "Returned **{}** allocated S points. You now have **{}** unspent S points.",
                result.returned_points, result.total_unspent_points
            )),
            footer: Some(format!("Cost: {} JC", result.cost)),
            ..EmbedModel::default()
        },
        ephemeral: true,
    })
}

/// Boundary used by `/dig help` after the channel check succeeds.
pub trait DigHelpPort {
    fn defer_ephemeral(&mut self) -> bool;
    fn actor_is_registered(&mut self) -> bool;
    fn target_is_registered(&mut self) -> bool;
    fn help_tunnel(&mut self);
    fn send_registration_error(&mut self);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigHelpOutcome {
    DeferFailed,
    RegistrationFailed,
    HelpAttempted,
}

/// Preserve the live callback's acknowledgement-before-repository ordering.
pub fn run_dig_help(port: &mut dyn DigHelpPort) -> DigHelpOutcome {
    if !port.defer_ephemeral() {
        return DigHelpOutcome::DeferFailed;
    }
    if !port.actor_is_registered() || !port.target_is_registered() {
        port.send_registration_error();
        return DigHelpOutcome::RegistrationFailed;
    }
    port.help_tunnel();
    DigHelpOutcome::HelpAttempted
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GearPickerEntry {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GearPickerPage {
    pub page: usize,
    pub page_count: usize,
    pub entries: Vec<GearPickerEntry>,
    pub previous_disabled: bool,
    pub next_disabled: bool,
}

#[must_use]
pub fn gear_picker_page(entries: &[GearPickerEntry], requested_page: usize) -> GearPickerPage {
    let page_count = entries.len().div_ceil(DISCORD_SELECT_OPTION_LIMIT).max(1);
    let page = requested_page.min(page_count - 1);
    let start = page * DISCORD_SELECT_OPTION_LIMIT;
    let end = (start + DISCORD_SELECT_OPTION_LIMIT).min(entries.len());
    GearPickerPage {
        page,
        page_count,
        entries: entries[start..end].to_vec(),
        previous_disabled: page == 0,
        next_disabled: page + 1 == page_count,
    }
}

#[must_use]
pub fn gear_panel_actions() -> Vec<&'static str> {
    vec![
        "Equip Gear",
        "Unequip Gear",
        "Repair Gear",
        "Recycle Relics",
    ]
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecyclableRelic {
    pub id: i64,
    pub name: String,
    pub rarity: String,
    pub recyclable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecycleRelicSelect {
    pub min_values: usize,
    pub max_values: usize,
    pub entries: Vec<GearPickerEntry>,
}

#[must_use]
pub fn recycle_relic_select(relics: &[RecyclableRelic]) -> RecycleRelicSelect {
    RecycleRelicSelect {
        min_values: 3,
        max_values: 3,
        entries: relics
            .iter()
            .filter(|relic| relic.recyclable)
            .map(|relic| GearPickerEntry {
                value: relic.id.to_string(),
                label: format!("[{}] {}", relic.rarity, relic.name),
            })
            .collect(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrestigeClickOutcome {
    Applied,
    DuplicatePrivateReply,
    WrongUserPrivateReply,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrestigeResolutionGuard {
    owner_id: i64,
    resolved: bool,
}

impl PrestigeResolutionGuard {
    #[must_use]
    pub const fn new(owner_id: i64) -> Self {
        Self {
            owner_id,
            resolved: false,
        }
    }

    #[must_use]
    pub const fn is_resolved(&self) -> bool {
        self.resolved
    }

    pub fn click(
        &mut self,
        interaction_user_id: i64,
        mut apply_prestige: impl FnMut(),
    ) -> PrestigeClickOutcome {
        if interaction_user_id != self.owner_id {
            return PrestigeClickOutcome::WrongUserPrivateReply;
        }
        if self.resolved {
            return PrestigeClickOutcome::DuplicatePrivateReply;
        }
        // Fence before invoking the service so a second callback cannot enter
        // while the first callback is awaiting downstream Discord work.
        self.resolved = true;
        apply_prestige();
        PrestigeClickOutcome::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeHelpPort {
        defer_succeeds: bool,
        actor_registered: bool,
        target_registered: bool,
        calls: Vec<&'static str>,
    }

    impl DigHelpPort for FakeHelpPort {
        fn defer_ephemeral(&mut self) -> bool {
            self.calls.push("defer");
            self.defer_succeeds
        }

        fn actor_is_registered(&mut self) -> bool {
            self.calls.push("actor");
            self.actor_registered
        }

        fn target_is_registered(&mut self) -> bool {
            self.calls.push("target");
            self.target_registered
        }

        fn help_tunnel(&mut self) {
            self.calls.push("help");
        }

        fn send_registration_error(&mut self) {
            self.calls.push("private_registration_error");
        }
    }

    #[test]
    fn test_dig_respec_debits_and_surfaces_returned_points() {
        let reply = render_respec_result(&RespecCommandResult {
            success: true,
            cost: 50,
            returned_points: 5,
            total_unspent_points: 12,
        })
        .expect("successful respec renders a reply");
        assert_eq!(reply.embed.title.as_deref(), Some("S Points Reset"));
        let description = reply.embed.description.expect("description");
        assert!(description.contains('5'));
        assert!(description.contains("12"));
        assert_eq!(reply.embed.footer.as_deref(), Some("Cost: 50 JC"));
        assert!(reply.ephemeral);
    }

    #[test]
    fn test_dig_help_stops_when_defer_fails() {
        let mut port = FakeHelpPort::default();
        assert_eq!(run_dig_help(&mut port), DigHelpOutcome::DeferFailed);
        assert_eq!(port.calls, ["defer"]);
    }

    #[test]
    fn test_dig_help_registration_error_uses_deferred_followup() {
        let mut port = FakeHelpPort {
            defer_succeeds: true,
            actor_registered: false,
            target_registered: true,
            ..FakeHelpPort::default()
        };
        assert_eq!(run_dig_help(&mut port), DigHelpOutcome::RegistrationFailed);
        assert_eq!(port.calls, ["defer", "actor", "private_registration_error"]);
    }

    #[test]
    fn test_gear_picker_paginates_every_manageable_item() {
        let entries = (0..30)
            .map(|index| GearPickerEntry {
                value: format!("gear:{index}"),
                label: format!("Gear {index:02}"),
            })
            .chain((0..5).map(|index| GearPickerEntry {
                value: format!("relic:{index}"),
                label: format!("Relic {index:02}"),
            }))
            .collect::<Vec<_>>();
        let first = gear_picker_page(&entries, 0);
        let second = gear_picker_page(&entries, 1);
        assert_eq!(first.page_count, 2);
        assert_eq!(first.entries.len(), 25);
        assert_eq!(second.entries.len(), 10);
        let first_values = first
            .entries
            .iter()
            .map(|entry| entry.value.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let second_values = second
            .entries
            .iter()
            .map(|entry| entry.value.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(first_values.is_disjoint(&second_values));
        assert_eq!(first_values.union(&second_values).count(), 35);
        assert!(first.previous_disabled);
        assert!(!first.next_disabled);
        assert!(!second.previous_disabled);
        assert!(second.next_disabled);
    }

    #[test]
    fn test_gear_panel_exposes_recycle_button() {
        assert!(gear_panel_actions().contains(&"Recycle Relics"));
    }

    #[test]
    fn test_recycle_view_requires_three_relics_and_labels_rarity() {
        let relics = (1..=3)
            .map(|id| RecyclableRelic {
                id,
                name: format!("Relic {id}"),
                rarity: "Common".to_owned(),
                recyclable: true,
            })
            .collect::<Vec<_>>();
        let select = recycle_relic_select(&relics);
        assert_eq!((select.min_values, select.max_values), (3, 3));
        assert!(
            select
                .entries
                .iter()
                .all(|entry| entry.label.starts_with("[Common]"))
        );
    }

    #[test]
    fn test_perk_callback_ignores_double_click() {
        let mut guard = PrestigeResolutionGuard::new(42);
        let mut service_calls = 0;
        assert_eq!(
            guard.click(42, || service_calls += 1),
            PrestigeClickOutcome::Applied
        );
        assert!(guard.is_resolved());
        assert_eq!(service_calls, 1);
        assert_eq!(
            guard.click(42, || service_calls += 1),
            PrestigeClickOutcome::DuplicatePrivateReply
        );
        assert_eq!(service_calls, 1);
    }

    #[test]
    fn wrong_user_cannot_consume_the_prestige_guard() {
        let mut guard = PrestigeResolutionGuard::new(42);
        assert_eq!(
            guard.click(7, || panic!("wrong user must not call the service")),
            PrestigeClickOutcome::WrongUserPrivateReply
        );
        assert!(!guard.is_resolved());
    }
}
