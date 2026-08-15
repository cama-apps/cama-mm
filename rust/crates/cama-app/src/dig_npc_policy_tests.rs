use super::*;
use std::collections::BTreeSet;

#[test]
fn roster_is_non_empty() {
    assert!(!NPCS.is_empty());
}

#[test]
fn every_roster_id_resolves_to_the_same_npc() {
    for npc in NPCS {
        assert_eq!(npc_by_id(npc.npc_id()), Some(*npc));
    }
}

#[test]
fn npc_ids_are_unique() {
    let ids = NPCS.iter().map(|npc| npc.npc_id()).collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), NPCS.len());
}

#[test]
fn npc_ids_are_snake_case_tokens() {
    for npc in NPCS {
        let id = npc.npc_id();
        assert!(!id.is_empty());
        assert_eq!(id, id.to_ascii_lowercase());
        assert!(!id.contains(' '));
        assert!(id.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        }));
    }
}

#[test]
fn every_npc_uses_a_valid_voice() {
    for npc in NPCS {
        assert!(VALID_VOICES.contains(&npc.voice()), "{}", npc.npc_id());
    }
}

#[test]
fn all_three_tone_profiles_are_represented() {
    let present = NPCS.iter().map(|npc| npc.voice()).collect::<BTreeSet<_>>();
    let expected = VALID_VOICES.into_iter().collect::<BTreeSet<_>>();
    assert_eq!(present, expected);
}

#[test]
fn titles_are_non_blank_and_begin_with_the() {
    for npc in NPCS {
        assert!(!npc.title().trim().is_empty());
        assert!(npc.title().starts_with("the "));
    }
}

#[test]
fn triggers_are_non_blank() {
    for npc in NPCS {
        assert!(!npc.triggers().trim().is_empty());
    }
}

#[test]
fn sample_lines_are_present_and_non_blank() {
    for npc in NPCS {
        assert!(!npc.sample_lines().is_empty());
        for line in npc.sample_lines() {
            assert!(!line.trim().is_empty());
        }
    }
}

#[test]
fn npc_is_an_immutable_copy_value() {
    const FIRST: DigNpc = NPCS[0];
    let copied = FIRST;
    assert_eq!(copied, FIRST);
    assert_eq!(copied.npc_id(), "the_surveyor");
}

#[test]
fn roster_lines_have_one_entry_per_npc() {
    assert_eq!(roster_lines().len(), NPCS.len());
}

#[test]
fn roster_line_format_includes_id_title_voice_and_triggers() {
    let lines = roster_lines();
    for npc in NPCS {
        let expected = format!(
            "- {} ({}, {}): {}",
            npc.npc_id(),
            npc.title(),
            npc.voice(),
            npc.triggers()
        );
        assert!(lines.contains(&expected));
    }
}

#[test]
fn roster_lines_are_bullet_prefixed() {
    for line in roster_lines() {
        assert!(line.starts_with("- "));
    }
}

#[test]
fn every_npc_id_is_recoverable_from_output() {
    let joined = roster_lines().join("\n");
    for npc in NPCS {
        assert!(joined.contains(npc.npc_id()));
    }
}
