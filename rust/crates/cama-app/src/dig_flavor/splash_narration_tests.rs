use std::sync::{Arc, Mutex};

use super::*;

const DIGGER_ID: i64 = 30_001;
const VICTIM_ID: i64 = 30_002;
const GUILD_ID: i64 = 42;

struct RecordingSplashAi {
    requests: Mutex<Vec<DigFlavorRequest>>,
    response: Result<DigToolCallResult, String>,
}

impl RecordingSplashAi {
    fn narrative(narrative: &str) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            response: Ok(DigToolCallResult {
                tool_name: NARRATE_SPLASH_TOOL_NAME.to_owned(),
                tool_args: FlavorToolArgs {
                    narrative: Some(narrative.to_owned()),
                    ..FlavorToolArgs::default()
                },
            }),
        }
    }

    fn request_count(&self) -> usize {
        self.requests.lock().expect("request mutex").len()
    }

    fn request(&self) -> DigFlavorRequest {
        self.requests.lock().expect("request mutex")[0].clone()
    }
}

impl DigFlavorAiPort for RecordingSplashAi {
    fn call_with_tools(&self, request: DigFlavorRequest) -> Result<DigToolCallResult, String> {
        self.requests
            .lock()
            .map_err(|error| error.to_string())?
            .push(request);
        self.response.clone()
    }
}

struct EmptyContext;

impl DigFlavorContextPort for EmptyContext {
    fn build(&self, _discord_id: i64, _guild_id: i64) -> Result<DigDmContext, String> {
        Ok(DigDmContext::default())
    }
}

struct SplashConfig(bool);

impl DigFlavorConfigPort for SplashConfig {
    fn ai_enabled(&self, _guild_id: i64) -> Result<bool, String> {
        Ok(self.0)
    }
}

struct UnusedRng;

impl DigFlavorRng for UnusedRng {
    fn choose_tone(&mut self, _tone_count: usize) -> usize {
        0
    }

    fn sample(&mut self) -> f64 {
        0.0
    }
}

fn repository() -> Arc<MemoryDigFlavorRepository> {
    let repository = Arc::new(MemoryDigFlavorRepository::default());
    repository.register_player(DIGGER_ID, GUILD_ID, "Alice", 0);
    repository.register_player(VICTIM_ID, GUILD_ID, "Bob", 0);
    repository.create_tunnel(DIGGER_ID, GUILD_ID, "Alice's Tunnel");
    repository
}

fn service(
    ai: Arc<RecordingSplashAi>,
    repository: Arc<MemoryDigFlavorRepository>,
) -> DigFlavorService {
    DigFlavorService::new(ai, repository, Arc::new(EmptyContext), Box::new(UnusedRng))
}

fn input<'a>(victims: &'a [SplashVictim]) -> SplashNarrationInput<'a> {
    SplashNarrationInput {
        digger_id: DIGGER_ID,
        guild_id: GUILD_ID,
        event_name: "Wisp's Tether",
        event_description: "A drifting mote of light.",
        splash_mode: "grant",
        victims,
        digger_layer: Some("Crystal"),
    }
}

#[test]
fn test_returns_string_under_cap() {
    assert_eq!(
        validate_splash_narrative("Short and sweet."),
        "Short and sweet."
    );
}

#[test]
fn test_clamps_to_200_chars() {
    let narrative = validate_splash_narrative(&"x".repeat(500));
    assert!(narrative.chars().count() <= 200);
    assert!(narrative.ends_with("..."));
}

#[test]
fn test_blank_string_returns_empty() {
    assert_eq!(validate_splash_narrative("   "), "");
}

#[test]
fn test_strips_whitespace() {
    assert_eq!(validate_splash_narrative("  hi  "), "hi");
}

#[test]
fn test_messages_have_system_and_user() {
    let messages = build_splash_narration_messages(
        "Alice",
        "Crystal",
        "Wisp's Tether",
        "A drifting mote of light recognizes you.",
        "grant",
        &[NamedSplashVictim {
            name: "Bob".to_owned(),
            amount: 4,
        }],
    );
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[0].content, SPLASH_NARRATION_SYSTEM_PROMPT);
    assert_eq!(messages[1].role, "user");
}

#[test]
fn test_user_content_includes_victim_names() {
    let messages = build_splash_narration_messages(
        "Alice",
        "Abyss",
        "The Eye Opens",
        "A seam in the rock parts.",
        "steal",
        &[
            NamedSplashVictim {
                name: "Bob".to_owned(),
                amount: 28,
            },
            NamedSplashVictim {
                name: "Carol".to_owned(),
                amount: 4,
            },
        ],
    );
    for expected in ["Alice", "Bob", "Carol", "steal", "Abyss"] {
        assert!(messages[1].content.contains(expected));
    }
}

#[test]
fn test_handles_empty_victims_gracefully() {
    let messages = build_splash_narration_messages("Alice", "Dirt", "Test", "Test", "burn", &[]);
    assert!(messages[1].content.contains("(no resolvable names)"));
}

#[test]
fn test_returns_narrative_on_success() {
    let ai = Arc::new(RecordingSplashAi::narrative(
        "Something gentle and luminous happens.",
    ));
    let victims = [SplashVictim {
        discord_id: VICTIM_ID,
        amount: 4,
    }];
    let output = service(ai.clone(), repository()).narrate_splash(input(&victims));
    assert_eq!(output, "Something gentle and luminous happens.");
    let request = ai.request();
    assert_eq!(request.feature, "dig.splash");
    assert_eq!(request.timeout_millis, 3_000);
    assert_eq!(request.max_tokens, 200);
    assert_eq!(request.tool, SPLASH_NARRATION_TOOL);
}

#[test]
fn test_empty_victims_returns_empty_without_calling_llm() {
    let ai = Arc::new(RecordingSplashAi::narrative("unused"));
    assert_eq!(
        service(ai.clone(), repository()).narrate_splash(input(&[])),
        ""
    );
    assert_eq!(ai.request_count(), 0);
}

#[test]
fn test_guild_ai_disable_skips_splash_llm() {
    let ai = Arc::new(RecordingSplashAi::narrative("unused"));
    let victims = [SplashVictim {
        discord_id: VICTIM_ID,
        amount: 5,
    }];
    let flavor = service(ai.clone(), repository()).with_config(Arc::new(SplashConfig(false)));
    assert_eq!(flavor.narrate_splash(input(&victims)), "");
    assert_eq!(ai.request_count(), 0);
}

fn assert_ai_error_falls_back(message: &str) {
    let ai = Arc::new(RecordingSplashAi {
        requests: Mutex::new(Vec::new()),
        response: Err(message.to_owned()),
    });
    let victims = [SplashVictim {
        discord_id: VICTIM_ID,
        amount: 10,
    }];
    assert_eq!(
        service(ai, repository()).narrate_splash(input(&victims)),
        ""
    );
}

#[test]
fn test_returns_empty_on_any_error_side_effect0() {
    assert_ai_error_falls_back("timeout");
}

#[test]
fn test_returns_empty_on_any_error_side_effect1() {
    assert_ai_error_falls_back("API down");
}

#[test]
fn test_returns_empty_on_any_error_side_effect2() {
    assert_ai_error_falls_back("bad json");
}

#[test]
fn test_returns_empty_on_wrong_tool() {
    let ai = Arc::new(RecordingSplashAi {
        requests: Mutex::new(Vec::new()),
        response: Ok(DigToolCallResult {
            tool_name: "other_tool".to_owned(),
            tool_args: FlavorToolArgs::default(),
        }),
    });
    let victims = [SplashVictim {
        discord_id: VICTIM_ID,
        amount: 5,
    }];
    assert_eq!(
        service(ai, repository()).narrate_splash(input(&victims)),
        ""
    );
}

#[test]
fn test_clamps_oversize_narrative() {
    let ai = Arc::new(RecordingSplashAi::narrative(&"y".repeat(600)));
    let victims = [SplashVictim {
        discord_id: VICTIM_ID,
        amount: 10,
    }];
    let output = service(ai, repository()).narrate_splash(input(&victims));
    assert!(output.chars().count() <= 200);
}

#[test]
fn test_renders_narrative_above_victim_lines() {
    let lines = splash_aftermath_lines(&SplashAftermath {
        mode: "steal".to_owned(),
        victims: vec![SplashVictim {
            discord_id: 99_001,
            amount: 10,
        }],
        llm_narrative: Some("Bob feels something missing.".to_owned()),
        ..SplashAftermath::default()
    });
    assert_eq!(lines[0], "*Bob feels something missing.*");
    assert!(lines[1].contains("<@99001>"));
    assert!(lines[1].contains("-10"));
}

#[test]
fn test_renders_without_narrative_when_missing() {
    let lines = splash_aftermath_lines(&SplashAftermath {
        mode: "grant".to_owned(),
        victims: vec![SplashVictim {
            discord_id: 99_002,
            amount: 4,
        }],
        ..SplashAftermath::default()
    });
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("<@99002>"));
    assert!(lines[0].contains("+4"));
}

#[test]
fn test_blank_narrative_treated_as_missing() {
    let lines = splash_aftermath_lines(&SplashAftermath {
        mode: "burn".to_owned(),
        victims: vec![SplashVictim {
            discord_id: 99_003,
            amount: 5,
        }],
        llm_narrative: Some("   ".to_owned()),
        ..SplashAftermath::default()
    });
    assert_eq!(lines.len(), 1);
    assert!(!lines[0].contains('*'));
}

#[test]
fn test_tool_has_required_shape() {
    assert_eq!(SPLASH_NARRATION_TOOL.kind, "function");
    assert_eq!(SPLASH_NARRATION_TOOL.name, "narrate_splash");
    assert!(SPLASH_NARRATION_TOOL.properties.contains(&"narrative"));
    assert!(SPLASH_NARRATION_TOOL.required.contains(&"narrative"));
}
