use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use super::*;

const USER_ID: i64 = 10_001;
const GUILD_ID: i64 = 123;

#[derive(Clone, Copy)]
enum FlameChannelMode {
    Send,
    MissingSend,
    SynchronousFailure,
}

#[derive(Clone)]
struct RecordingFlameChannel {
    sent: Arc<Mutex<Vec<String>>>,
    mode: FlameChannelMode,
}

impl RecordingFlameChannel {
    fn new(mode: FlameChannelMode) -> (Self, Arc<Mutex<Vec<String>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                sent: Arc::clone(&sent),
                mode,
            },
            sent,
        )
    }
}

impl AtmosphericChannel for RecordingFlameChannel {
    fn send(&self, content: String) -> Result<Option<FlameSendFuture>, FlameSendError> {
        match self.mode {
            FlameChannelMode::Send => {
                self.sent.lock().expect("flame channel lock").push(content);
                Ok(Some(Box::pin(async { Ok(()) })))
            }
            FlameChannelMode::MissingSend => Ok(None),
            FlameChannelMode::SynchronousFailure => Err(FlameSendError),
        }
    }
}

#[derive(Default)]
struct RecordingFlameSpawner {
    tasks: Mutex<Vec<FlameSendFuture>>,
}

impl AtmosphericTaskSpawner for RecordingFlameSpawner {
    fn spawn(&self, task: FlameSendFuture) -> Result<(), FlameTaskError> {
        self.tasks.lock().expect("flame spawner lock").push(task);
        Ok(())
    }
}

fn recorded_flame_messages(sent: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    sent.lock().expect("recorded flame lock").clone()
}

// tests/test_dig_flame.py::TestCatastrophicLinePool::test_pool_is_non_empty
#[test]
fn dig_flame_pool_is_non_empty() {
    assert!(!CATASTROPHIC_LINES.is_empty());
}

// tests/test_dig_flame.py::TestCatastrophicLinePool::test_lines_are_non_blank_strings
#[test]
fn dig_flame_lines_are_non_blank_strings() {
    for line in CATASTROPHIC_LINES {
        assert!(!line.trim().is_empty());
    }
}

// tests/test_dig_flame.py::TestCatastrophicLinePool::test_lines_are_unique
#[test]
fn dig_flame_lines_are_unique() {
    let unique = CATASTROPHIC_LINES.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(unique.len(), CATASTROPHIC_LINES.len());
}

// tests/test_dig_flame.py::TestCatastrophicLinePool::test_lines_contain_no_proper_nouns_marker
#[test]
fn dig_flame_lines_contain_no_mechanics_markers() {
    let banned = ["luminosity", "prestige", "jc", "jopacoin", "cave-in chance"];
    for line in CATASTROPHIC_LINES {
        let lowered = line.to_ascii_lowercase();
        for term in banned {
            assert!(!lowered.contains(term), "{term:?} leaked in {line:?}");
        }
    }
}

// tests/test_dig_flame.py::TestCatastrophicLinePool::test_pick_returns_member_of_pool
#[test]
fn dig_flame_pick_returns_member_of_pool() {
    let mut rng = SeededDigFlavorRng::new(1);
    for _ in 0..100 {
        assert!(CATASTROPHIC_LINES.contains(&pick_catastrophic_line(&mut rng)));
    }
}

// tests/test_dig_flame.py::TestCatastrophicLinePool::test_pick_is_seeded_deterministic
#[test]
fn dig_flame_pick_is_seeded_deterministic() {
    let mut first_rng = SeededDigFlavorRng::new(1_234);
    let first = pick_catastrophic_line(&mut first_rng);
    let mut second_rng = SeededDigFlavorRng::new(1_234);
    assert_eq!(pick_catastrophic_line(&mut second_rng), first);
}

// tests/test_dig_flame.py::TestCatastrophicLinePool::test_pick_covers_multiple_lines
#[test]
fn dig_flame_pick_covers_multiple_lines() {
    let mut rng = SeededDigFlavorRng::new(99);
    let seen = (0..400)
        .map(|_| pick_catastrophic_line(&mut rng))
        .collect::<BTreeSet<_>>();
    assert!(seen.len() > 1);
}

// tests/test_dig_flame.py::TestPostAtmospheric::test_formats_with_prefix_and_italics
#[test]
fn dig_flame_formats_with_prefix_and_italics() {
    let (channel, sent) = RecordingFlameChannel::new(FlameChannelMode::Send);
    let spawner = RecordingFlameSpawner::default();
    post_atmospheric(Some(&channel), Some(&spawner), "X", "a tunnel folds shut");
    assert_eq!(recorded_flame_messages(&sent), ["X *a tunnel folds shut*"]);
    assert_eq!(spawner.tasks.lock().expect("spawner lock").len(), 1);
}

// tests/test_dig_flame.py::TestPostAtmospheric::test_strips_surrounding_whitespace_from_line
#[test]
fn dig_flame_strips_surrounding_whitespace_from_line() {
    let (channel, sent) = RecordingFlameChannel::new(FlameChannelMode::Send);
    let spawner = RecordingFlameSpawner::default();
    post_atmospheric(Some(&channel), Some(&spawner), "P", "   spaced line   ");
    assert_eq!(recorded_flame_messages(&sent), ["P *spaced line*"]);
}

// tests/test_dig_flame.py::TestPostAtmospheric::test_none_channel_is_a_noop
#[test]
fn dig_flame_none_channel_is_a_noop() {
    let spawner = RecordingFlameSpawner::default();
    post_atmospheric::<RecordingFlameChannel, _>(None, Some(&spawner), "P", "line");
    assert!(spawner.tasks.lock().expect("spawner lock").is_empty());
}

// tests/test_dig_flame.py::TestPostAtmospheric::test_empty_line_is_a_noop
#[test]
fn dig_flame_empty_line_is_a_noop() {
    let (channel, sent) = RecordingFlameChannel::new(FlameChannelMode::Send);
    let spawner = RecordingFlameSpawner::default();
    post_atmospheric(Some(&channel), Some(&spawner), "P", "");
    assert!(recorded_flame_messages(&sent).is_empty());
    assert!(spawner.tasks.lock().expect("spawner lock").is_empty());
}

// tests/test_dig_flame.py::TestPostAtmospheric::test_channel_without_send_is_a_noop
#[test]
fn dig_flame_channel_without_send_is_a_noop() {
    let (channel, sent) = RecordingFlameChannel::new(FlameChannelMode::MissingSend);
    let spawner = RecordingFlameSpawner::default();
    post_atmospheric(Some(&channel), Some(&spawner), "P", "line");
    assert!(recorded_flame_messages(&sent).is_empty());
    assert!(spawner.tasks.lock().expect("spawner lock").is_empty());
}

// tests/test_dig_flame.py::TestPostAtmospheric::test_send_raising_is_swallowed
#[test]
fn dig_flame_send_raising_is_swallowed() {
    let (channel, sent) = RecordingFlameChannel::new(FlameChannelMode::SynchronousFailure);
    let spawner = RecordingFlameSpawner::default();
    post_atmospheric(Some(&channel), Some(&spawner), "P", "line");
    assert!(recorded_flame_messages(&sent).is_empty());
    assert!(spawner.tasks.lock().expect("spawner lock").is_empty());
}

// tests/test_dig_flame.py::TestPostAtmospheric::test_no_running_loop_is_swallowed
#[test]
fn dig_flame_no_running_loop_is_swallowed() {
    let (channel, sent) = RecordingFlameChannel::new(FlameChannelMode::Send);
    post_atmospheric::<_, RecordingFlameSpawner>(Some(&channel), None, "P", "line");
    assert_eq!(recorded_flame_messages(&sent), ["P *line*"]);
}

// tests/test_dig_flame.py::TestPostCatastrophic::test_uses_catastrophic_prefix_and_pool_line
#[test]
fn dig_flame_catastrophic_post_uses_prefix_and_pool_line() {
    let mut expected_rng = SeededDigFlavorRng::new(7);
    let expected = pick_catastrophic_line(&mut expected_rng);
    let (channel, sent) = RecordingFlameChannel::new(FlameChannelMode::Send);
    let spawner = RecordingFlameSpawner::default();
    let mut rng = SeededDigFlavorRng::new(7);
    post_catastrophic(Some(&channel), Some(&spawner), &mut rng);
    assert_eq!(
        recorded_flame_messages(&sent),
        [format!("{CATASTROPHIC_PREFIX} *{expected}*")]
    );
}

// tests/test_dig_flame.py::TestPostCatastrophic::test_none_channel_is_a_noop
#[test]
fn dig_flame_catastrophic_none_channel_is_a_noop() {
    let spawner = RecordingFlameSpawner::default();
    let mut rng = SeededDigFlavorRng::new(1);
    post_catastrophic::<RecordingFlameChannel, _, _>(None, Some(&spawner), &mut rng);
    assert!(spawner.tasks.lock().expect("spawner lock").is_empty());
}

// tests/test_dig_npcs.py::TestRosterIntegrity::test_roster_is_non_empty
#[test]
fn dig_npcs_roster_is_non_empty() {
    assert!(!NPCS.is_empty());
}

// tests/test_dig_npcs.py::TestRosterIntegrity::test_dict_key_matches_npc_id
#[test]
fn dig_npcs_each_canonical_id_resolves_to_same_npc() {
    for npc in NPCS {
        assert_eq!(npc_by_id(npc.npc_id()), Some(*npc));
    }
}

// tests/test_dig_npcs.py::TestRosterIntegrity::test_npc_ids_are_unique
#[test]
fn dig_npcs_ids_are_unique() {
    let ids = NPCS.iter().map(|npc| npc.npc_id()).collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), NPCS.len());
}

// tests/test_dig_npcs.py::TestRosterIntegrity::test_npc_ids_are_snake_case_tokens
#[test]
fn dig_npcs_ids_are_snake_case_tokens() {
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

// tests/test_dig_npcs.py::TestRosterIntegrity::test_every_npc_uses_a_valid_voice
#[test]
fn dig_npcs_every_npc_uses_a_valid_voice() {
    for npc in NPCS {
        assert!(VALID_VOICES.contains(&npc.voice()), "{}", npc.npc_id());
    }
}

// tests/test_dig_npcs.py::TestRosterIntegrity::test_all_three_tone_profiles_are_represented
#[test]
fn dig_npcs_all_three_tone_profiles_are_represented() {
    let present = NPCS.iter().map(|npc| npc.voice()).collect::<BTreeSet<_>>();
    assert_eq!(present, VALID_VOICES.into_iter().collect::<BTreeSet<_>>());
}

// tests/test_dig_npcs.py::TestRosterIntegrity::test_titles_are_non_blank_and_carry_no_proper_names
#[test]
fn dig_npcs_titles_are_non_blank_and_begin_with_the() {
    for npc in NPCS {
        assert!(!npc.title().trim().is_empty());
        assert!(npc.title().starts_with("the "));
    }
}

// tests/test_dig_npcs.py::TestRosterIntegrity::test_triggers_are_non_blank
#[test]
fn dig_npcs_triggers_are_non_blank() {
    for npc in NPCS {
        assert!(!npc.triggers().trim().is_empty());
    }
}

// tests/test_dig_npcs.py::TestRosterIntegrity::test_sample_lines_present_and_well_formed
#[test]
fn dig_npcs_sample_lines_are_present_and_well_formed() {
    for npc in NPCS {
        assert!(!npc.sample_lines().is_empty());
        for line in npc.sample_lines() {
            assert!(!line.trim().is_empty());
        }
    }
}

// tests/test_dig_npcs.py::TestRosterIntegrity::test_npc_is_frozen_dataclass
#[test]
fn dig_npcs_npc_is_an_immutable_copy_value() {
    const FIRST: DigNpc = NPCS[0];
    let copied = FIRST;
    assert_eq!(copied, FIRST);
    assert_eq!(copied.npc_id(), "the_surveyor");
}

// tests/test_dig_npcs.py::TestRosterLines::test_one_line_per_npc
#[test]
fn dig_npcs_roster_lines_have_one_entry_per_npc() {
    assert_eq!(roster_lines().len(), NPCS.len());
}

// tests/test_dig_npcs.py::TestRosterLines::test_line_format_includes_id_title_voice_and_triggers
#[test]
fn dig_npcs_roster_line_format_includes_all_fields() {
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

// tests/test_dig_npcs.py::TestRosterLines::test_lines_are_bullet_prefixed
#[test]
fn dig_npcs_roster_lines_are_bullet_prefixed() {
    for line in roster_lines() {
        assert!(line.starts_with("- "));
    }
}

// tests/test_dig_npcs.py::TestRosterLines::test_every_npc_id_is_recoverable_from_output
#[test]
fn dig_npcs_every_id_is_recoverable_from_output() {
    let joined = roster_lines().join("\n");
    for npc in NPCS {
        assert!(joined.contains(npc.npc_id()));
    }
}

fn histogram(entries: &[(&str, i64)]) -> BTreeMap<String, i64> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), *value))
        .collect()
}

macro_rules! play_style_case {
    ($name:ident, $entries:expr, $expected:expr) => {
        #[test]
        fn $name() {
            assert_eq!(classify_play_style(&histogram($entries)), $expected);
        }
    };
}

play_style_case!(
    test_classify_play_style_histogram0_unknown,
    &[("safe", 2), ("risky", 1)],
    "unknown"
);
play_style_case!(test_classify_play_style_histogram1_unknown, &[], "unknown");
play_style_case!(
    test_classify_play_style_histogram2_cautious_grinder,
    &[("safe", 80), ("risky", 10), ("desperate", 1)],
    "cautious_grinder"
);
play_style_case!(
    test_classify_play_style_histogram3_reckless_degen,
    &[("safe", 10), ("risky", 20), ("desperate", 10)],
    "reckless_degen"
);
play_style_case!(
    test_classify_play_style_histogram4_calculated_risk_taker,
    &[("safe", 10), ("risky", 50), ("desperate", 5)],
    "calculated_risk_taker"
);
play_style_case!(
    test_classify_play_style_histogram5_social_butterfly,
    &[("safe", 5), ("risky", 5), ("help", 20)],
    "social_butterfly"
);
play_style_case!(
    test_classify_play_style_histogram6_balanced_explorer,
    &[("safe", 10), ("risky", 8), ("desperate", 2)],
    "balanced_explorer"
);

fn validation_context() -> (BTreeSet<String>, &'static str, f64) {
    (
        BTreeSet::from(["crystal_golem".to_owned(), "void_seam".to_owned()]),
        "industrial_grim",
        5.0,
    )
}

fn valid_args() -> FlavorToolArgs {
    FlavorToolArgs {
        narrative: Some("The walls weep dust.".to_owned()),
        tone: Some("industrial_grim".to_owned()),
        ..FlavorToolArgs::default()
    }
}

#[test]
fn test_valid_minimal_args_returns_result() {
    let (events, tone, cap) = validation_context();
    let result = validate_flavor_args(&valid_args(), &events, tone, cap).expect("valid payload");
    assert_eq!(result.narrative, "The walls weep dust.");
    assert_eq!(result.tone, "industrial_grim");
    assert_eq!(result.flavor_bonus_pct, 0.0);
    assert_eq!(result.npc_appearance, None);
    assert_eq!(result.picked_event_id, None);
    assert_eq!(result.memory_update, None);
}

#[test]
fn test_empty_narrative_rejected() {
    let (events, tone, cap) = validation_context();
    let mut args = valid_args();
    args.narrative = Some("  ".to_owned());
    assert_eq!(validate_flavor_args(&args, &events, tone, cap), None);
}

#[test]
fn test_numbers_in_narrative_rejected() {
    let (events, tone, cap) = validation_context();
    let mut args = valid_args();
    args.narrative = Some("You earned 5 JC.".to_owned());
    assert_eq!(validate_flavor_args(&args, &events, tone, cap), None);
}

#[test]
fn test_long_narrative_truncated() {
    let (events, _, cap) = validation_context();
    let mut args = valid_args();
    args.narrative = Some("x ".repeat(400));
    args.tone = Some("cosmic_dread".to_owned());
    let result = validate_flavor_args(&args, &events, "cosmic_dread", cap).expect("valid payload");
    assert!(result.narrative.chars().count() <= NARRATIVE_MAX_CHARS);
    assert!(result.narrative.ends_with("..."));
}

#[test]
fn test_invalid_tone_falls_back_to_rolled() {
    let (events, _, cap) = validation_context();
    let mut args = valid_args();
    args.tone = Some("made_up_tone".to_owned());
    let result = validate_flavor_args(&args, &events, "cosmic_dread", cap).expect("valid payload");
    assert_eq!(result.tone, "cosmic_dread");
}

macro_rules! bonus_clamp_case {
    ($name:ident, $raw:expr, $cap:expr, $expected:expr) => {
        #[test]
        fn $name() {
            let (events, tone, _) = validation_context();
            let mut args = valid_args();
            args.flavor_bonus_pct = Some($raw);
            let result = validate_flavor_args(&args, &events, tone, $cap).expect("valid payload");
            assert_eq!(result.flavor_bonus_pct, $expected);
        }
    };
}

bonus_clamp_case!(test_flavor_bonus_clamped_to_cap_3_0_5_0_3_0, 3.0, 5.0, 3.0);
bonus_clamp_case!(
    test_flavor_bonus_clamped_to_cap_12_0_5_0_5_0,
    12.0,
    5.0,
    5.0
);
bonus_clamp_case!(
    test_flavor_bonus_clamped_to_cap_negative_12_0_5_0_negative_5_0,
    -12.0,
    5.0,
    -5.0
);
bonus_clamp_case!(
    test_flavor_bonus_clamped_to_cap_8_5_10_0_8_5,
    8.5,
    10.0,
    8.5
);
bonus_clamp_case!(
    test_flavor_bonus_clamped_to_cap_15_0_10_0_10_0,
    15.0,
    10.0,
    10.0
);

#[test]
fn test_picked_event_must_be_in_allow_list() {
    let (events, tone, cap) = validation_context();
    let mut args = valid_args();
    args.picked_event_id = Some("made_up_event".to_owned());
    let result = validate_flavor_args(&args, &events, tone, cap).expect("valid payload");
    assert_eq!(result.picked_event_id, None);
}

#[test]
fn test_picked_event_passes_when_in_list() {
    let (events, tone, cap) = validation_context();
    let mut args = valid_args();
    args.picked_event_id = Some("crystal_golem".to_owned());
    let result = validate_flavor_args(&args, &events, tone, cap).expect("valid payload");
    assert_eq!(result.picked_event_id.as_deref(), Some("crystal_golem"));
}

#[test]
fn test_npc_appearance_renders() {
    let (events, tone, cap) = validation_context();
    let mut args = valid_args();
    args.npc_appearance = Some(NpcAppearance {
        id_or_name: "the_old_hand".to_owned(),
        line: "Walked away from worse.".to_owned(),
    });
    let result = validate_flavor_args(&args, &events, tone, cap).expect("valid payload");
    assert_eq!(result.npc_appearance, args.npc_appearance);
}

#[test]
fn test_npc_appearance_missing_line_dropped() {
    let (events, tone, cap) = validation_context();
    let mut args = valid_args();
    args.npc_appearance = Some(NpcAppearance {
        id_or_name: "the_old_hand".to_owned(),
        line: String::new(),
    });
    let result = validate_flavor_args(&args, &events, tone, cap).expect("valid payload");
    assert_eq!(result.npc_appearance, None);
}

#[test]
fn test_memory_update_truncated_to_byte_cap() {
    let (events, tone, cap) = validation_context();
    let mut args = valid_args();
    args.memory_update = Some("🪨".repeat(MEMORY_MAX_BYTES));
    let result = validate_flavor_args(&args, &events, tone, cap).expect("valid payload");
    assert!(result.memory_update.expect("memory").len() <= MEMORY_MAX_BYTES);
}

#[test]
fn test_under_cap_passes() {
    assert_eq!(
        validate_splash_narrative("Bob feels something missing."),
        "Bob feels something missing."
    );
}

#[test]
fn test_truncates_over_cap() {
    assert!(validate_splash_narrative(&"y".repeat(600)).chars().count() <= 200);
}

#[test]
fn test_blank_returns_empty() {
    assert_eq!(validate_splash_narrative("   "), "");
}

#[test]
fn test_narrate_dig_tool_has_expected_shape() {
    assert_eq!(NARRATE_DIG_TOOL.kind, "function");
    assert_eq!(NARRATE_DIG_TOOL.name, "narrate_dig");
    for field in NARRATE_DIG_PROPERTIES {
        assert!(NARRATE_DIG_TOOL.properties.contains(&field));
    }
    assert_eq!(
        NARRATE_DIG_TOOL
            .required
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["narrative", "tone"])
    );
}

#[test]
fn test_narrate_dig_ai_schema_preserves_python_nested_contract() {
    let tool = narrate_dig_ai_tool_definition();
    assert_eq!(tool.name, NARRATE_DIG_TOOL_NAME);
    assert_eq!(tool.required, vec!["narrative", "tone"]);
    assert_eq!(tool.additional_properties, None);

    let bonus = tool
        .properties
        .iter()
        .find(|property| property.name == "flavor_bonus_pct")
        .expect("numeric flavor bonus property");
    assert_eq!(bonus.schema, AiToolPropertySchema::Number);

    let npc = tool
        .properties
        .iter()
        .find(|property| property.name == "npc_appearance")
        .expect("nested NPC property");
    let AiToolPropertySchema::Object {
        properties,
        required,
        additional_properties,
    } = &npc.schema
    else {
        panic!("npc_appearance must be an object schema");
    };
    assert_eq!(
        properties
            .iter()
            .map(|property| property.name.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["id_or_name", "line"])
    );
    // Python leaves the nested object open and does not declare nested
    // required fields; post-call validation requires the complete pair.
    assert!(required.is_empty());
    assert_eq!(*additional_properties, None);
}

#[test]
fn test_play_style_descriptions_complete() {
    assert_eq!(
        PLAY_STYLE_DESCRIPTIONS
            .iter()
            .map(|(style, _)| *style)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "cautious_grinder",
            "reckless_degen",
            "calculated_risk_taker",
            "balanced_explorer",
            "social_butterfly",
            "unknown",
        ])
    );
}

#[test]
fn test_tone_profiles_have_three_voices() {
    assert_eq!(
        TONE_PROFILES
            .iter()
            .map(|(tone, _)| *tone)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["cosmic_dread", "industrial_grim", "cryptic_folkloric"])
    );
}

macro_rules! system_prompt_case {
    ($name:ident, $tone:expr) => {
        #[test]
        fn $name() {
            let prompt = build_dig_flavor_system_prompt($tone, 5.0);
            assert!(prompt.contains($tone));
            assert!(prompt.contains("±5%"));
            assert!(build_dig_flavor_system_prompt($tone, 10.0).contains("±10%"));
        }
    };
}

system_prompt_case!(
    test_system_prompt_injects_tone_and_cap_cosmic_dread,
    "cosmic_dread"
);
system_prompt_case!(
    test_system_prompt_injects_tone_and_cap_industrial_grim,
    "industrial_grim"
);
system_prompt_case!(
    test_system_prompt_injects_tone_and_cap_cryptic_folkloric,
    "cryptic_folkloric"
);

#[test]
fn test_basic_tunnel_includes_depth_tier_and_balance() {
    let tunnel = TunnelFlavorState {
        tunnel_name: "Test Tunnel".to_owned(),
        depth: 42,
        pickaxe_tier: 2,
        prestige_level: 1,
        luminosity: 80,
        streak_days: 5,
        total_digs: 20,
        ..TunnelFlavorState::default()
    };
    let context = build_player_state_context(&tunnel, 50);
    for expected in ["Test Tunnel", "42", "Stone", "Iron", "50 JC"] {
        assert!(
            context.contains(expected),
            "missing {expected:?}: {context}"
        );
    }
}

#[test]
fn test_empty_tunnel_uses_defaults() {
    let context = build_player_state_context(&TunnelFlavorState::default(), 0);
    assert!(context.contains("Dirt"));
    assert!(context.contains("Wooden"));
}

#[test]
fn test_blank_personality_falls_back_to_default_blurb_none() {
    assert_eq!(
        build_personality_context(None),
        "New player, no history yet."
    );
}

#[test]
fn test_blank_personality_falls_back_to_default_blurb_personality1() {
    assert_eq!(
        build_personality_context(Some(&DigPersonality::default())),
        "New player, no history yet."
    );
}

#[test]
fn test_play_style_and_histogram_render() {
    let personality = DigPersonality {
        play_style: "balanced_explorer".to_owned(),
        choice_histogram: histogram(&[("safe", 10), ("risky", 8)]),
        notable_moments: vec!["Slew their first boss".to_owned()],
        ..DigPersonality::default()
    };
    let context = build_personality_context(Some(&personality));
    assert!(context.contains("balanced_explorer"));
    assert!(context.contains("safe: 10"));
    assert!(context.contains("first boss"));
}

macro_rules! outcome_context_case {
    ($name:ident, $outcome:expr, $tokens:expr) => {
        #[test]
        fn $name() {
            let context = build_dig_outcome_context(&$outcome);
            for token in $tokens {
                assert!(context.contains(token), "missing {token:?}: {context}");
            }
        }
    };
}

outcome_context_case!(
    test_dig_outcome_renders_expected_tokens_result0_must_contain0,
    DigFlavorOutcome {
        advance: 3,
        jc_earned: 5,
        ..DigFlavorOutcome::default()
    },
    ["+3 blocks", "JC earned: 5"]
);
outcome_context_case!(
    test_dig_outcome_renders_expected_tokens_result1_must_contain1,
    DigFlavorOutcome {
        cave_in: true,
        cave_in_detail: Some(CaveInFlavorDetail {
            block_loss: 5,
            message: "Rocks tumbled.".to_owned(),
            kind: String::new(),
        }),
        ..DigFlavorOutcome::default()
    },
    ["CAVE-IN", "5 blocks"]
);
outcome_context_case!(
    test_dig_outcome_renders_expected_tokens_result2_must_contain2,
    DigFlavorOutcome::default(),
    ["+0 blocks"]
);

#[test]
fn test_structure_includes_state_personality_outcome() {
    let messages = build_messages(
        "system",
        "state",
        "personality",
        "outcome",
        OptionalMessageSections::default(),
    );
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[0].content, "system");
    for label in ["PLAYER STATE", "PERSONALITY", "DIG OUTCOME"] {
        assert!(messages[1].content.contains(label));
    }
}

#[test]
fn test_optional_sections_render() {
    let messages = build_messages(
        "system",
        "state",
        "personality",
        "outcome",
        OptionalMessageSections {
            multiplayer: "sabotage stuff",
            history: "some history",
            dm_context: "some memory",
            eligible_events: "ev_a, ev_b",
        },
    );
    for label in [
        "MULTIPLAYER",
        "DIG HISTORY",
        "DM CONTEXT",
        "ELIGIBLE EVENTS",
    ] {
        assert!(messages[1].content.contains(label));
    }
}

#[test]
fn test_blank_actions_returns_empty() {
    assert_eq!(build_multiplayer_context(&[], &BTreeMap::new()), "");
}

#[test]
fn test_actions_render_with_serialized_details() {
    let action = DigAction {
        action_type: "sabotage".to_owned(),
        actor_id: 111,
        target_id: Some(222),
        detail: SocialActionDetail {
            damage: 5,
            target_id: Some(222),
            ..SocialActionDetail::default()
        },
        ..DigAction::default()
    };
    assert!(build_multiplayer_context(&[action], &BTreeMap::new()).contains("Sabotage"));
}

#[test]
fn test_blank_history_returns_empty() {
    assert_eq!(
        build_dig_history_context(&[], &TunnelFlavorState::default()),
        ""
    );
}

#[test]
fn test_lifetime_stats_render() {
    let tunnel = TunnelFlavorState {
        total_digs: 50,
        total_jc_earned: 200,
        max_depth: 60,
        ..TunnelFlavorState::default()
    };
    let context = build_dig_history_context(&[], &tunnel);
    assert!(context.contains("50 digs"));
    assert!(context.contains("200 JC"));
}

#[test]
fn test_multiplayer_state_matches_python_labels_and_rank() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    let tunnel = TunnelFlavorState {
        cheer_data: Some(format!(
            "[{{\"cheerer_id\":1,\"expires_at\":{}}}]",
            now + 3_600
        )),
        revenge_until: now + 7_200,
        revenge_type: "sabotage".to_owned(),
        trap_active: true,
        insured_until: now + 3_600,
        reinforced_until: now + 3_600,
        injury_state: Some(r#"{"type":"fracture","digs_remaining":2}"#.to_owned()),
        ..TunnelFlavorState::default()
    };
    let context = build_multiplayer_context_with_state(
        &[],
        &tunnel,
        3,
        &BTreeMap::from([(1, "The Cheerer".to_owned())]),
    );
    for token in [
        "Cheers: The Cheerer",
        "Revenge: sabotage boost active",
        "Trap: armed",
        "Protection: insured, reinforced",
        "Injury: fracture (2 digs remaining)",
        "Rank: #3 in guild",
    ] {
        assert!(context.contains(token), "missing {token:?}: {context}");
    }
}

#[test]
fn test_history_renders_cavein_trend_boss_and_events() {
    let tunnel = TunnelFlavorState {
        max_depth: 80,
        total_digs: 10,
        total_jc_earned: 40,
        prestige_level: 2,
        best_run_score: 11,
        boss_progress: BTreeMap::from([(String::from("25"), String::from("defeated"))]),
        boss_attempts: 2,
        ..TunnelFlavorState::default()
    };
    let actions = vec![
        DigAction {
            action_type: "dig".to_owned(),
            depth_before: 10,
            depth_after: 12,
            detail: SocialActionDetail {
                cave_in: true,
                block_loss: 4,
                ..SocialActionDetail::default()
            },
            ..DigAction::default()
        },
        DigAction {
            action_type: "event".to_owned(),
            detail: SocialActionDetail {
                event_id: Some("root_event".to_owned()),
                ..SocialActionDetail::default()
            },
            ..DigAction::default()
        },
        DigAction {
            action_type: "boss_fight".to_owned(),
            detail: SocialActionDetail {
                won: Some(true),
                risk: Some("risky".to_owned()),
                boundary: Some(25),
                ..SocialActionDetail::default()
            },
            ..DigAction::default()
        },
    ];
    let context = build_dig_history_context(&actions, &tunnel);
    for token in [
        "Last 1 digs: 0 advances, 1 cave-ins",
        "last cave-in: 1 digs ago, -4 blocks",
        "Depth trend: 10->12 (+2)",
        "Record: 80",
        "Lifetime: 10 digs, 40 JC earned",
        "Bosses:",
        "Prestige: level 2, best run score 11",
        "Recent events: root_event",
        "Boss fights: won vs Grothak the Unbreakable",
    ] {
        assert!(context.contains(token), "missing {token:?}: {context}");
    }
}

#[test]
fn test_normal_and_boss_outcomes_render_all_python_projection_fields() {
    let normal = DigFlavorOutcome {
        boss_encounter: true,
        boss_info: Some(BossFlavorInfo {
            name: "The Gatekeeper".to_owned(),
        }),
        artifact: Some(ArtifactFlavorInfo {
            name: "Old Tooth".to_owned(),
        }),
        luminosity_info: Some(LuminosityFlavorInfo {
            level: "dim".to_owned(),
            current: 42,
            drained: 3,
        }),
        weather: Some(FlavorWeather {
            name: "Rain".to_owned(),
            description: "The roof leaks.".to_owned(),
        }),
        corruption: Some("The Stain".to_owned()),
        mutations: vec!["glass bones".to_owned()],
        ..DigFlavorOutcome::default()
    };
    let context = build_dig_outcome_context(&normal);
    for token in [
        "BOSS ENCOUNTER: The Gatekeeper",
        "Artifact found: Old Tooth",
        "Luminosity: 42/100 (dim), drained 3",
        "Weather: Rain - The roof leaks.",
        "Corruption: The Stain",
        "Active mutations: glass bones",
    ] {
        assert!(context.contains(token), "missing {token:?}: {context}");
    }

    let win = DigFlavorOutcome {
        won: true,
        boss_name: "The Gatekeeper".to_owned(),
        boundary: 25,
        risk_tier: "risky".to_owned(),
        win_chance: 0.75,
        payout: 12,
        stat_point_awarded: true,
        dialogue: "The gate remembers.".to_owned(),
        ..DigFlavorOutcome::default()
    };
    let win_context = build_boss_outcome_context(&win);
    for token in [
        "Result: VICTORY",
        "JC earned: +12",
        "FIRST CLEAR BONUS: +1 S stat point!",
        "Boss final words: \"The gate remembers.\"",
    ] {
        assert!(
            win_context.contains(token),
            "missing {token:?}: {win_context}"
        );
    }

    let loss = DigFlavorOutcome {
        boss_name: "The Gatekeeper".to_owned(),
        boundary: 25,
        risk_tier: "cautious".to_owned(),
        win_chance: 0.25,
        jc_delta: -7,
        knockback: 9,
        new_depth: 16,
        phase: 2,
        ..DigFlavorOutcome::default()
    };
    let loss_context = build_boss_outcome_context(&loss);
    for token in [
        "Result: DEFEAT",
        "JC lost: -7",
        "Knocked back: 9 blocks",
        "New depth: 16",
        "This was the PHASE 2 (harder) form of the boss!",
    ] {
        assert!(
            loss_context.contains(token),
            "missing {token:?}: {loss_context}"
        );
    }
    assert!(!loss_context.contains("JC earned"));
}

struct RecordingAi {
    requests: Mutex<Vec<DigFlavorRequest>>,
    response: Result<DigToolCallResult, String>,
}

#[derive(Default)]
struct ReceiptMemoryRepository {
    receipt: Mutex<Option<FlavorDeliveryReceipt>>,
    apply_calls: Mutex<usize>,
}

impl DigFlavorDataPort for ReceiptMemoryRepository {
    fn gather(&self, _discord_id: i64, _guild_id: i64) -> Result<GatheredFlavorData, String> {
        Ok(GatheredFlavorData::default())
    }

    fn add_balance(&self, _discord_id: i64, _guild_id: i64, _delta: i64) -> Result<(), String> {
        Ok(())
    }

    fn set_dm_memory(&self, _discord_id: i64, _guild_id: i64, _value: &str) -> Result<(), String> {
        Ok(())
    }

    fn get_personality(
        &self,
        _discord_id: i64,
        _guild_id: i64,
    ) -> Result<Option<DigPersonality>, String> {
        Ok(None)
    }

    fn upsert_personality(
        &self,
        _discord_id: i64,
        _guild_id: i64,
        _personality: DigPersonality,
    ) -> Result<(), String> {
        Ok(())
    }

    fn get_flavor_receipt(
        &self,
        _dig_action_id: i64,
        _discord_id: i64,
        _guild_id: i64,
    ) -> Result<Option<FlavorDeliveryReceipt>, String> {
        Ok(self.receipt.lock().expect("receipt mutex").clone())
    }

    fn apply_flavor_receipt(
        &self,
        receipt: FlavorDeliveryReceipt,
        _discord_id: i64,
        _guild_id: i64,
    ) -> Result<FlavorDeliveryReceipt, String> {
        *self.apply_calls.lock().expect("apply calls mutex") += 1;
        *self.receipt.lock().expect("receipt mutex") = Some(receipt.clone());
        Ok(receipt)
    }
}

impl RecordingAi {
    fn success(args: FlavorToolArgs) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            response: Ok(DigToolCallResult {
                tool_name: NARRATE_DIG_TOOL_NAME.to_owned(),
                tool_args: args,
            }),
        }
    }

    fn request_count(&self) -> usize {
        self.requests.lock().expect("requests mutex").len()
    }

    fn request(&self, index: usize) -> DigFlavorRequest {
        self.requests.lock().expect("requests mutex")[index].clone()
    }
}

impl DigFlavorAiPort for RecordingAi {
    fn call_with_tools(&self, request: DigFlavorRequest) -> Result<DigToolCallResult, String> {
        self.requests
            .lock()
            .map_err(|error| error.to_string())?
            .push(request);
        self.response.clone()
    }
}

#[derive(Clone)]
struct FixedContext {
    calls: Arc<Mutex<usize>>,
}

impl Default for FixedContext {
    fn default() -> Self {
        Self {
            calls: Arc::new(Mutex::new(0)),
        }
    }
}

impl DigFlavorContextPort for FixedContext {
    fn build(&self, _discord_id: i64, _guild_id: i64) -> Result<DigDmContext, String> {
        *self.calls.lock().map_err(|error| error.to_string())? += 1;
        Ok(DigDmContext {
            last_match_summary: "No recent inhouse matches.".to_owned(),
            economy_state: "No outstanding loan.".to_owned(),
            server_activity: "Server is quiet.".to_owned(),
            ..DigDmContext::default()
        })
    }
}

struct FixedConfig(bool);

impl DigFlavorConfigPort for FixedConfig {
    fn ai_enabled(&self, _guild_id: i64) -> Result<bool, String> {
        Ok(self.0)
    }
}

struct FixedRng;

impl DigFlavorRng for FixedRng {
    fn choose_tone(&mut self, _tone_count: usize) -> usize {
        1
    }

    fn sample(&mut self) -> f64 {
        0.84
    }
}

fn default_ai_args() -> FlavorToolArgs {
    FlavorToolArgs {
        narrative: Some("The walls listen. They have always listened.".to_owned()),
        tone: Some("cryptic_folkloric".to_owned()),
        ..FlavorToolArgs::default()
    }
}

fn registered_repository(balance: i64) -> Arc<MemoryDigFlavorRepository> {
    let repository = Arc::new(MemoryDigFlavorRepository::default());
    repository.register_player(USER_ID, GUILD_ID, "Player10001", balance);
    repository.create_tunnel(USER_ID, GUILD_ID, "Test Tunnel");
    repository
}

fn service_with(
    ai: Arc<RecordingAi>,
    repository: Arc<MemoryDigFlavorRepository>,
    context: Arc<FixedContext>,
) -> DigFlavorService {
    DigFlavorService::new(ai, repository, context, Box::new(FixedRng))
}

#[test]
fn test_flavor_replays_durable_receipt_without_second_ai_call() {
    let repository = Arc::new(ReceiptMemoryRepository {
        receipt: Mutex::new(Some(FlavorDeliveryReceipt {
            source_action_id: 77,
            state: FlavorDeliveryState::Applied,
            narrative: Some("The seam answers twice.".to_owned()),
            tone: Some("industrial_grim".to_owned()),
            callback_reference: Some("old dust".to_owned()),
            npc_appearance: Some(FlavorDeliveryNpcAppearance {
                id_or_name: "the_old_hand".to_owned(),
                line: "Keep the lamp low.".to_owned(),
            }),
            picked_event_id: Some("old_seam".to_owned()),
            memory_update: Some("The seam answered twice.".to_owned()),
            bonus: FlavorBonusReceipt { delta: 3 },
        })),
        ..ReceiptMemoryRepository::default()
    });
    let ai = Arc::new(RecordingAi::success(FlavorToolArgs {
        narrative: Some("This must never be requested.".to_owned()),
        tone: Some("industrial_grim".to_owned()),
        ..FlavorToolArgs::default()
    }));
    let service = DigFlavorService::new(
        ai.clone(),
        repository.clone(),
        Arc::new(FixedContext::default()),
        Box::new(FixedRng),
    );
    let mut result = DigFlavorOutcome {
        dig_action_id: Some(77),
        jc_earned: 15,
        depth_after: 10,
        ..DigFlavorOutcome::default()
    };
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(ai.request_count(), 0);
    assert_eq!(
        result.llm_narrative.as_deref(),
        Some("The seam answers twice.")
    );
    assert_eq!(result.llm_jc_delta, Some(3));
    assert_eq!(result.llm_picked_event_id.as_deref(), Some("old_seam"));
    assert_eq!(*repository.apply_calls.lock().expect("apply calls"), 0);
}

fn ordinary_outcome(jc_earned: i64) -> DigFlavorOutcome {
    DigFlavorOutcome {
        success: true,
        advance: 3,
        jc_earned,
        depth_after: 10,
        ..DigFlavorOutcome::default()
    }
}

#[test]
fn test_flavor_adds_narrative_keys() {
    let repository = registered_repository(100);
    let ai = Arc::new(RecordingAi::success(default_ai_args()));
    let service = service_with(ai.clone(), repository, Arc::new(FixedContext::default()));
    let mut result = ordinary_outcome(5);
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(
        result.llm_narrative.as_deref(),
        Some("The walls listen. They have always listened.")
    );
    assert_eq!(result.llm_tone.as_deref(), Some("cryptic_folkloric"));
    let request = ai.request(0);
    assert_eq!(request.feature, "dig.flavor");
    assert_eq!(request.timeout_millis, 5_000);
}

#[test]
fn test_flavor_skips_parked_boss_reopen() {
    let repository = Arc::new(MemoryDigFlavorRepository::default());
    let ai = Arc::new(RecordingAi::success(default_ai_args()));
    let context = Arc::new(FixedContext::default());
    let service = service_with(ai.clone(), repository, context.clone());
    let mut result = DigFlavorOutcome {
        success: true,
        dig_consumed: Some(false),
        boss_pending: true,
        ..DigFlavorOutcome::default()
    };
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(ai.request_count(), 0);
    assert_eq!(*context.calls.lock().expect("context calls"), 0);
}

#[test]
fn test_flavor_honors_guild_ai_disable() {
    let repository = Arc::new(MemoryDigFlavorRepository::default());
    let ai = Arc::new(RecordingAi::success(default_ai_args()));
    let context = Arc::new(FixedContext::default());
    let service = service_with(ai.clone(), repository, context.clone())
        .with_config(Arc::new(FixedConfig(false)));
    let mut result = ordinary_outcome(4);
    result.dig_consumed = Some(true);
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(ai.request_count(), 0);
    assert_eq!(*context.calls.lock().expect("context calls"), 0);
}

fn assert_flavor_error_falls_back(message: &str) {
    let repository = registered_repository(100);
    let ai = Arc::new(RecordingAi {
        requests: Mutex::new(Vec::new()),
        response: Err(message.to_owned()),
    });
    let service = service_with(ai, repository, Arc::new(FixedContext::default()));
    let mut result = ordinary_outcome(5);
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(result.llm_narrative, None);
    assert_eq!(result.advance, 3);
}

#[test]
fn test_flavor_falls_back_silently_on_error_side_effect0() {
    assert_flavor_error_falls_back("timeout");
}

#[test]
fn test_flavor_falls_back_silently_on_error_side_effect1() {
    assert_flavor_error_falls_back("API down");
}

#[test]
fn test_flavor_falls_back_on_wrong_tool() {
    let repository = registered_repository(100);
    let ai = Arc::new(RecordingAi {
        requests: Mutex::new(Vec::new()),
        response: Ok(DigToolCallResult {
            tool_name: "other_tool".to_owned(),
            tool_args: FlavorToolArgs::default(),
        }),
    });
    let service = service_with(ai, repository, Arc::new(FixedContext::default()));
    let mut result = ordinary_outcome(2);
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(result.llm_narrative, None);
}

#[test]
fn test_flavor_falls_back_on_numbers_in_narrative() {
    let repository = registered_repository(100);
    let ai = Arc::new(RecordingAi::success(FlavorToolArgs {
        narrative: Some("You earned 5 JC.".to_owned()),
        tone: Some("industrial_grim".to_owned()),
        ..FlavorToolArgs::default()
    }));
    let service = service_with(ai, repository, Arc::new(FixedContext::default()));
    let mut result = ordinary_outcome(5);
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(result.llm_narrative, None);
}

fn bonus_service(percent: f64, balance: i64) -> (DigFlavorService, Arc<MemoryDigFlavorRepository>) {
    let repository = registered_repository(balance);
    let ai = Arc::new(RecordingAi::success(FlavorToolArgs {
        narrative: Some("Pickaxe sings against the seam.".to_owned()),
        tone: Some("industrial_grim".to_owned()),
        flavor_bonus_pct: Some(percent),
        ..FlavorToolArgs::default()
    }));
    (
        service_with(ai, repository.clone(), Arc::new(FixedContext::default())),
        repository,
    )
}

#[test]
fn test_flavor_bonus_credits_balance_when_within_cap() {
    let (service, repository) = bonus_service(4.0, 100);
    let mut result = ordinary_outcome(15);
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(result.llm_jc_delta, Some(1));
    assert_eq!(repository.balance(USER_ID, GUILD_ID), 101);
}

#[test]
fn test_flavor_positive_bonus_clamped_at_nonstreak_cap() {
    let (service, repository) = bonus_service(4.0, 100);
    let mut result = ordinary_outcome(20);
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(result.llm_jc_delta, None);
    assert_eq!(repository.balance(USER_ID, GUILD_ID), 100);
}

#[test]
fn test_flavor_headroom_follows_lifted_cap_under_yield_buff() {
    let (service, repository) = bonus_service(4.0, 100);
    let mut result = ordinary_outcome(30);
    result.nonstreak_jc = Some(30);
    result.nonstreak_cap = Some(35);
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(result.llm_jc_delta, Some(1));
    assert_eq!(repository.balance(USER_ID, GUILD_ID), 101);
}

#[test]
fn test_flavor_headroom_ignores_milestone_and_streak() {
    let (service, repository) = bonus_service(4.0, 100);
    let mut result = ordinary_outcome(100);
    result.milestone_bonus = 85;
    result.streak_bonus = 5;
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(result.llm_jc_delta, Some(4));
    assert_eq!(repository.balance(USER_ID, GUILD_ID), 104);
}

#[test]
fn test_flavor_clamp_uses_pretax_nonstreak_basis() {
    let (service, repository) = bonus_service(4.0, 100);
    let mut result = ordinary_outcome(162);
    result.nonstreak_jc = Some(20);
    result.milestone_bonus = 150;
    result.streak_bonus = 10;
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(result.llm_jc_delta, None);
    assert_eq!(repository.balance(USER_ID, GUILD_ID), 100);
}

#[test]
fn test_flavor_negative_bonus_applies_past_cap() {
    let (service, repository) = bonus_service(-4.0, 100);
    let mut result = ordinary_outcome(20);
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(result.llm_jc_delta, Some(-1));
    assert_eq!(repository.balance(USER_ID, GUILD_ID), 99);
}

#[test]
fn test_flavor_bonus_not_clamped_for_boss() {
    let (service, repository) = bonus_service(4.0, 100);
    let mut result = ordinary_outcome(100);
    result.won = true;
    result.boss_name = "Test Boss".to_owned();
    result.boundary = 25;
    result.risk_tier = "cautious".to_owned();
    result.win_chance = 0.5;
    service.flavor(&mut result, USER_ID, GUILD_ID, true);
    assert_eq!(result.llm_jc_delta, Some(4));
    assert_eq!(repository.balance(USER_ID, GUILD_ID), 104);
}

#[test]
fn test_flavor_bonus_skipped_when_jc_earned_zero() {
    let (service, repository) = bonus_service(5.0, 100);
    let mut result = ordinary_outcome(0);
    result.advance = 0;
    result.cave_in = true;
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert_eq!(result.llm_jc_delta, None);
    assert_eq!(repository.balance(USER_ID, GUILD_ID), 100);
}

#[test]
fn test_flavor_writes_dm_memory() {
    let repository = registered_repository(100);
    let ai = Arc::new(RecordingAi::success(FlavorToolArgs {
        narrative: Some("First descent. The depth notes you.".to_owned()),
        tone: Some("cosmic_dread".to_owned()),
        memory_update: Some("Player crossed first threshold quietly. The dark noticed.".to_owned()),
        ..FlavorToolArgs::default()
    }));
    let service = service_with(ai, repository.clone(), Arc::new(FixedContext::default()));
    let mut result = ordinary_outcome(1);
    service.flavor(&mut result, USER_ID, GUILD_ID, false);
    assert!(
        repository
            .dm_memory(USER_ID, GUILD_ID)
            .contains("first threshold")
    );
}

#[test]
fn test_update_personality_creates_new() {
    let repository = registered_repository(100);
    let service = service_with(
        Arc::new(RecordingAi::success(default_ai_args())),
        repository.clone(),
        Arc::new(FixedContext::default()),
    );
    service.update_personality(USER_ID, GUILD_ID, Some("safe"), None);
    assert_eq!(
        repository
            .personality(USER_ID, GUILD_ID)
            .expect("personality")
            .choice_histogram
            .get("safe"),
        Some(&1)
    );
}

#[test]
fn test_update_personality_records_notable_moment() {
    let repository = registered_repository(100);
    let service = service_with(
        Arc::new(RecordingAi::success(default_ai_args())),
        repository.clone(),
        Arc::new(FixedContext::default()),
    );
    service.update_personality(
        USER_ID,
        GUILD_ID,
        None,
        Some(&BTreeMap::from([("first_boss_kill".to_owned(), true)])),
    );
    let moments = repository
        .personality(USER_ID, GUILD_ID)
        .expect("personality")
        .notable_moments;
    assert_eq!(moments.len(), 1);
    assert!(moments[0].to_lowercase().contains("first boss"));
}

#[test]
fn test_invalid_choice_ignored() {
    let repository = registered_repository(100);
    let service = service_with(
        Arc::new(RecordingAi::success(default_ai_args())),
        repository.clone(),
        Arc::new(FixedContext::default()),
    );
    service.update_personality(USER_ID, GUILD_ID, Some("invalid_choice"), None);
    assert!(
        !repository
            .personality(USER_ID, GUILD_ID)
            .expect("personality")
            .choice_histogram
            .contains_key("invalid_choice")
    );
}

#[test]
fn test_get_returns_empty_when_missing() {
    assert_eq!(
        MemoryDigFlavorRepository::default().dm_memory(99_999, GUILD_ID),
        ""
    );
}

#[test]
fn test_set_then_get_round_trip() {
    let repository = MemoryDigFlavorRepository::default();
    repository.write_dm_memory(99_001, GUILD_ID, "Notable moment recorded.");
    assert_eq!(
        repository.dm_memory(99_001, GUILD_ID),
        "Notable moment recorded."
    );
}

#[test]
fn test_overwrite_replaces_text() {
    let repository = MemoryDigFlavorRepository::default();
    repository.write_dm_memory(99_001, GUILD_ID, "First note.");
    repository.write_dm_memory(99_001, GUILD_ID, "Second note.");
    assert_eq!(repository.dm_memory(99_001, GUILD_ID), "Second note.");
}

#[test]
fn test_2kb_truncation_byte_safe() {
    let repository = MemoryDigFlavorRepository::default();
    repository.write_dm_memory(99_001, GUILD_ID, &"🪨".repeat(5_000));
    assert!(repository.dm_memory(99_001, GUILD_ID).len() <= MEMORY_MAX_BYTES);
}

#[test]
fn test_per_guild_isolation() {
    let repository = MemoryDigFlavorRepository::default();
    repository.write_dm_memory(99_001, GUILD_ID, "guild A memory");
    repository.write_dm_memory(99_001, 999, "guild B memory");
    assert_eq!(repository.dm_memory(99_001, GUILD_ID), "guild A memory");
    assert_eq!(repository.dm_memory(99_001, 999), "guild B memory");
}

#[test]
fn test_get_personality_none_when_missing() {
    assert_eq!(
        MemoryDigFlavorRepository::default().personality(99_999, GUILD_ID),
        None
    );
}

#[test]
fn test_upsert_persists_and_round_trips() {
    let repository = MemoryDigFlavorRepository::default();
    repository.write_personality(
        USER_ID,
        GUILD_ID,
        DigPersonality {
            play_style: "cautious_grinder".to_owned(),
            choice_histogram: histogram(&[("safe", 10), ("risky", 3)]),
            notable_moments: vec!["Slew first boss".to_owned()],
            ..DigPersonality::default()
        },
    );
    let result = repository
        .personality(USER_ID, GUILD_ID)
        .expect("personality");
    assert_eq!(result.play_style, "cautious_grinder");
    assert_eq!(result.choice_histogram.get("safe"), Some(&10));
}

#[test]
fn test_empty_when_none_logged() {
    assert!(
        MemoryDigFlavorRepository::default()
            .recent_social_actions(USER_ID, GUILD_ID)
            .is_empty()
    );
}

#[test]
fn test_returns_only_social_actions() {
    let repository = MemoryDigFlavorRepository::default();
    repository.log_action(DigAction {
        guild_id: GUILD_ID,
        actor_id: USER_ID,
        target_id: Some(10_002),
        action_type: "sabotage".to_owned(),
        depth_before: 50,
        depth_after: 45,
        jc_delta: -5,
        ..DigAction::default()
    });
    repository.log_action(DigAction {
        guild_id: GUILD_ID,
        actor_id: USER_ID,
        action_type: "dig".to_owned(),
        depth_before: 48,
        depth_after: 50,
        ..DigAction::default()
    });
    assert_eq!(
        repository
            .recent_social_actions(USER_ID, GUILD_ID)
            .into_iter()
            .map(|action| action.action_type)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["sabotage".to_owned()])
    );
}

#[test]
fn test_returns_preconditions_for_normal_dig() {
    let tunnel = TunnelState {
        depth: 5,
        ..TunnelState::default()
    };
    let (terminal, preconditions) = dig_with_preconditions(Some(&tunnel));
    assert_eq!(terminal, None);
    assert_eq!(preconditions.map(|value| value.depth_before), Some(5));
}

#[test]
fn test_unregistered_returns_terminal_failure() {
    let (terminal, _) = dig_with_preconditions(None);
    let terminal = terminal.expect("terminal failure");
    assert!(!terminal.success);
}

#[test]
fn test_normal_outcome_advances_depth() {
    let mut tunnel = TunnelState {
        depth: 5,
        max_depth: 5,
        ..TunnelState::default()
    };
    let depth_before = tunnel.depth;
    let result = apply_resolved_dig_outcome(
        &mut tunnel,
        &ResolvedDigFlavorInput {
            advance: 2,
            jc_earned: 3,
            ..ResolvedDigFlavorInput::default()
        },
        1_000,
    );
    assert!(result.success);
    assert_eq!(result.advance, 2);
    assert_eq!(result.depth_after, depth_before + 2);
    assert_eq!(result.llm_narrative, None);
}

#[test]
fn test_cave_in_outcome_records_detail() {
    let mut tunnel = TunnelState {
        depth: 10,
        max_depth: 10,
        ..TunnelState::default()
    };
    let result = apply_resolved_dig_outcome(
        &mut tunnel,
        &ResolvedDigFlavorInput {
            cave_in: true,
            cave_in_block_loss: 4,
            cave_in_type: "stun".to_owned(),
            ..ResolvedDigFlavorInput::default()
        },
        1_000,
    );
    assert!(result.success);
    assert!(result.cave_in);
    assert_eq!(result.cave_in_detail.expect("cave-in detail").kind, "stun");
}
