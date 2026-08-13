//! Catalog messaging invariants shared by Rust event presentation.

use cama_app::dig_loot::canonical_event_catalog;

#[test]
fn dig_event_messaging_has_no_gain_flavor_on_jc_loss_outcomes() {
    let gain_phrases = ["spills your way", "spill your way"];
    let mut offenders = Vec::new();

    for event in canonical_event_catalog() {
        let choices = event
            .safe_option
            .iter()
            .chain(event.risky_option.iter())
            .chain(event.desperate_option.iter())
            .chain(event.steps.iter().flat_map(|step| step.choices.iter()));
        for choice in choices {
            for outcome in std::iter::once(&choice.success).chain(choice.failure.iter()) {
                let description = outcome.description.to_lowercase();
                if outcome.jc < 0
                    && gain_phrases
                        .iter()
                        .any(|phrase| description.contains(phrase))
                {
                    offenders.push(format!(
                        "{}: {:?} (jc={})",
                        event.id, outcome.description, outcome.jc
                    ));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "gain-flavored JC losses:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn dig_event_payload_projection_carries_ascii_art() {
    let events_with_art = canonical_event_catalog()
        .iter()
        .filter(|event| {
            event
                .ascii_art
                .as_deref()
                .is_some_and(|art| !art.is_empty())
        })
        .collect::<Vec<_>>();

    assert_eq!(events_with_art.len(), 71);
    let stream = events_with_art
        .iter()
        .find(|event| event.id == "underground_stream")
        .expect("stream art is projected");
    assert!(
        stream
            .ascii_art
            .as_deref()
            .unwrap()
            .contains("~~~~~~~~~~~~")
    );
}
