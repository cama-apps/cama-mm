//! Fail-soft duel herald narration over typed configuration and LLM ports.
//!
//! The module deliberately has no Discord or network dependency. Runtime code
//! can adapt its configuration store and completion provider to these ports,
//! while every policy decision remains deterministic and locally testable.

use crate::duel::{FlavorDetails, FlavorEvent, FlavorPort};
use thiserror::Error;

pub const SYSTEM_PROMPT: &str = concat!(
    "You are the herald of a challenge of honor in a Dota inhouse league, ",
    "announcing from a world of hedge knights in the spirit of the Tales of ",
    "Dunk and Egg: muddy tourney meadows, mystery knights, trials of seven, ",
    "and armor won on a wager — crossed with concise Dota references. Write ",
    "original courtly fanfare with sharp humor."
);

pub const PROMPT_CONSTRAINTS: &str = concat!(
    "Do not quote or imitate any existing novel or television dialogue. ",
    "Never invent or alter players, wagers, deadlines, trial types, or ",
    "outcomes. Produce one line under 300 characters and never include ",
    "mentions."
);

pub const HERALD_VOICES: [&str; 4] = [
    concat!(
        "Speak as a towering hedge knight errant: plainspoken, dutiful, short of ",
        "coin, weighing every quarrel against the price of a new shield."
    ),
    concat!(
        "Speak as a shaven-headed squire far too clever for his station: dry, ",
        "precise, quietly needling both duelists about their lane equilibrium."
    ),
    concat!(
        "Speak as the master of the games at a muddy tourney meadow: officious ",
        "and long-suffering, reading the lists while the smallfolk heckle item ",
        "builds."
    ),
    concat!(
        "Speak as a mystery knight in mismatched armor: theatrical and anonymous, ",
        "hinting the feud will be settled where the river runes spawn."
    ),
];

const ISSUED_FALLBACKS: [&str; 2] = [
    "📯 A gauntlet slaps the mud of the tourney meadow, and even the couriers stop to gawk.",
    "📯 A shield is hung upon the lists; let the smallfolk gather and the wards go out.",
];
const REMINDER_FALLBACKS: [&str; 2] = [
    "⏳ The herald calls across the meadow: answer, ser, before your courier grows a beard.",
    "⏳ The lists still wait on your word; a hedge knight would have answered by now.",
];
const ACCEPTED_COMBAT_FALLBACKS: [&str; 2] = [
    "⚔️ Lances lower in the mid lane; this quarrel will be settled before gods, men, and observer wards.",
    "⚔️ Trial by combat is sworn: one lane, two knights, and a river rune between them.",
];
const ACCEPTED_FIVE_FALLBACKS: [&str; 2] = [
    "🛡️ Five banners to a side, near enough a trial of seven for this muddy meadow.",
    "🛡️ The trial of five is sworn; may the drafting gods show mercy on both retinues.",
];
const DECLINED_FALLBACKS: [&str; 2] = [
    "🏳️ The gauntlet is returned with its seal unbroken; the meadow boos politely.",
    "🏳️ The shield comes down without a tilt, and the rolls record a careful knight.",
];
const EXPIRED_FALLBACKS: [&str; 2] = [
    "⌛ The challenge rusts on the lists like a puddle-painted shield left in the rain.",
    "⌛ No answer came; the herald strikes the tilt from the rolls at first light.",
];
const RESOLVED_FALLBACKS: [&str; 2] = [
    "🏆 The tilt is run; the victor's name goes in the rolls and the pot rides home in his saddlebags.",
    "🏆 The commons cheer, the loser mutters of buybacks, and the ledger closes another quarrel.",
];
const VOIDED_FALLBACKS: [&str; 2] = [
    "📜 The marshal voids the tilt; both purses ride home unbloodied.",
    "📜 No verdict and no victor; the stakes walk home like a courier spared.",
];
const UNRESOLVED_FALLBACKS: [&str; 2] = [
    "🗡️ The meadow still waits: two knights sworn to a tilt that no one has ridden.",
    "🗡️ The rolls show an unfinished trial; even the wards have expired waiting.",
];

pub const ALL_FLAVOR_EVENTS: [FlavorEvent; 9] = [
    FlavorEvent::Issued,
    FlavorEvent::Reminder,
    FlavorEvent::AcceptedCombat,
    FlavorEvent::AcceptedFive,
    FlavorEvent::Declined,
    FlavorEvent::Expired,
    FlavorEvent::Resolved,
    FlavorEvent::Voided,
    FlavorEvent::Unresolved,
];

pub fn fallbacks(event: FlavorEvent) -> &'static [&'static str; 2] {
    match event {
        FlavorEvent::Issued => &ISSUED_FALLBACKS,
        FlavorEvent::Reminder => &REMINDER_FALLBACKS,
        FlavorEvent::AcceptedCombat => &ACCEPTED_COMBAT_FALLBACKS,
        FlavorEvent::AcceptedFive => &ACCEPTED_FIVE_FALLBACKS,
        FlavorEvent::Declined => &DECLINED_FALLBACKS,
        FlavorEvent::Expired => &EXPIRED_FALLBACKS,
        FlavorEvent::Resolved => &RESOLVED_FALLBACKS,
        FlavorEvent::Voided => &VOIDED_FALLBACKS,
        FlavorEvent::Unresolved => &UNRESOLVED_FALLBACKS,
    }
}

fn event_name(event: FlavorEvent) -> &'static str {
    match event {
        FlavorEvent::Issued => "issued",
        FlavorEvent::Reminder => "reminder",
        FlavorEvent::AcceptedCombat => "accepted_combat",
        FlavorEvent::AcceptedFive => "accepted_five",
        FlavorEvent::Declined => "declined",
        FlavorEvent::Expired => "expired",
        FlavorEvent::Resolved => "resolved",
        FlavorEvent::Voided => "voided",
        FlavorEvent::Unresolved => "unresolved",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuelFlavorDetail {
    pub key: String,
    pub value: String,
}

impl DuelFlavorDetail {
    pub fn new(key: impl Into<String>, value: impl ToString) -> Self {
        Self {
            key: key.into(),
            value: value.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuelFlavorCompletionRequest {
    pub prompt: String,
    pub system_prompt: String,
    pub feature: String,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct DuelFlavorPortError {
    pub message: String,
}

impl DuelFlavorPortError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Offline boundary for the runtime's configured completion provider.
pub trait DuelFlavorLlmPort {
    fn complete(
        &mut self,
        request: DuelFlavorCompletionRequest,
    ) -> Result<Option<String>, DuelFlavorPortError>;
}

/// Offline boundary for the per-guild AI feature flag.
pub trait DuelFlavorConfigPort {
    fn ai_enabled(&mut self, guild_id: i64) -> Result<bool, DuelFlavorPortError>;
}

/// Choice boundary keeps fallback and herald selection deterministic in tests.
pub trait DuelFlavorChoicePort {
    fn choose_index(&mut self, option_count: usize) -> usize;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FirstDuelFlavorChoice;

impl DuelFlavorChoicePort for FirstDuelFlavorChoice {
    fn choose_index(&mut self, _option_count: usize) -> usize {
        0
    }
}

pub struct DuelFlavorService<L, C, S> {
    llm: Option<L>,
    config: Option<C>,
    selector: S,
}

impl<L, C, S> DuelFlavorService<L, C, S>
where
    L: DuelFlavorLlmPort,
    C: DuelFlavorConfigPort,
    S: DuelFlavorChoicePort,
{
    pub fn new(llm: Option<L>, config: Option<C>, selector: S) -> Self {
        Self {
            llm,
            config,
            selector,
        }
    }

    pub fn llm(&self) -> Option<&L> {
        self.llm.as_ref()
    }

    pub fn config(&self) -> Option<&C> {
        self.config.as_ref()
    }

    pub fn generate(
        &mut self,
        event: FlavorEvent,
        guild_id: i64,
        details: &[DuelFlavorDetail],
    ) -> String {
        if self.llm.is_none() {
            return self.fallback(event);
        }

        if let Some(config) = self.config.as_mut() {
            match config.ai_enabled(guild_id) {
                Ok(true) => {}
                Ok(false) | Err(_) => return self.fallback(event),
            }
        }

        let voice_index = self.selector.choose_index(HERALD_VOICES.len()) % HERALD_VOICES.len();
        let request = DuelFlavorCompletionRequest {
            prompt: build_prompt(event, details),
            system_prompt: format!(
                "{} {} {}",
                SYSTEM_PROMPT, HERALD_VOICES[voice_index], PROMPT_CONSTRAINTS
            ),
            feature: format!("duel.{}", event_name(event)),
        };

        let generated = match self
            .llm
            .as_mut()
            .expect("LLM presence checked above")
            .complete(request)
        {
            Ok(value) => value,
            Err(_) => return self.fallback(event),
        };
        let cleaned = generated
            .as_deref()
            .map(|value| sanitize_output(value, 300))
            .unwrap_or_default();
        if cleaned.is_empty() {
            self.fallback(event)
        } else {
            cleaned
        }
    }

    fn fallback(&mut self, event: FlavorEvent) -> String {
        let choices = fallbacks(event);
        let index = self.selector.choose_index(choices.len()) % choices.len();
        choices[index].to_owned()
    }
}

impl<L, C, S> FlavorPort for DuelFlavorService<L, C, S>
where
    L: DuelFlavorLlmPort,
    C: DuelFlavorConfigPort,
    S: DuelFlavorChoicePort,
{
    fn generate(&mut self, event: FlavorEvent, guild_id: i64, details: &FlavorDetails) -> String {
        let trial = details.trial.unwrap_or("None");
        let typed_details = [
            DuelFlavorDetail::new("challenge_id", details.challenge_id),
            DuelFlavorDetail::new("challenger", details.challenger),
            DuelFlavorDetail::new("recipient", details.recipient),
            DuelFlavorDetail::new("wager", details.wager),
            DuelFlavorDetail::new("issuance_fee", details.issuance_fee),
            DuelFlavorDetail::new("status", details.status),
            DuelFlavorDetail::new("trial", trial),
        ];
        DuelFlavorService::generate(self, event, guild_id, &typed_details)
    }
}

pub fn build_prompt(event: FlavorEvent, details: &[DuelFlavorDetail]) -> String {
    let rendered = details
        .iter()
        .map(|detail| {
            format!(
                "'{}': '{}'",
                python_string_escape(&detail.key),
                python_string_escape(&sanitize_detail(&detail.value))
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("Event: {}. Details: {{{rendered}}}.", event_name(event))
}

pub fn sanitize_detail(value: &str) -> String {
    value
        .chars()
        .filter(|character| !matches!(character, '<' | '>' | '@' | '`') && !character.is_control())
        .take(80)
        .collect()
}

fn python_string_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

pub fn sanitize_output(value: &str, max_length: usize) -> String {
    if value.is_empty() {
        return String::new();
    }

    let normalized = value.chars().map(compatibility_fold).map(|character| {
        if character.is_control() || is_unicode_format(character) {
            ' '
        } else {
            character
        }
    });
    let collapsed = normalized
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_length)
        .collect::<String>();
    if contains_link_or_markup(&collapsed) {
        String::new()
    } else {
        collapsed.replace('@', "＠")
    }
}

fn compatibility_fold(character: char) -> char {
    match character {
        '\u{3000}' => ' ',
        '\u{ff01}'..='\u{ff5e}' => {
            char::from_u32(u32::from(character) - 0xfee0).unwrap_or(character)
        }
        _ => character,
    }
}

fn is_unicode_format(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{0600}'..='\u{0605}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

pub fn contains_link_or_markup(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if lower.contains("://")
        || ["data:", "javascript:", "mailto:", "sms:", "tel:"]
            .iter()
            .any(|scheme| lower.contains(scheme))
        || contains_markdown_link(value)
        || contains_discord_markup(value)
    {
        return true;
    }

    value.split_whitespace().any(token_contains_network_address)
}

fn contains_markdown_link(value: &str) -> bool {
    value.match_indices('[').any(|(start, _)| {
        value[start + 1..]
            .find("](")
            .and_then(|offset| {
                let destination = start + 1 + offset + 2;
                value[destination..].find(')')
            })
            .is_some()
    })
}

fn contains_discord_markup(value: &str) -> bool {
    let mut rest = value;
    while let Some(start) = rest.find('<') {
        let candidate = &rest[start + 1..];
        let Some(end) = candidate.find('>') else {
            return false;
        };
        let body = &candidate[..end];
        if body.starts_with('@')
            || body.starts_with('#')
            || body.starts_with('/')
            || body.starts_with("t:")
            || body.starts_with(':')
            || body.starts_with("a:")
        {
            return true;
        }
        rest = &candidate[end + 1..];
    }
    false
}

fn token_contains_network_address(token: &str) -> bool {
    let candidate = token.trim_matches(|character: char| {
        matches!(
            character,
            ',' | '!' | ';' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
        )
    });
    let authority = candidate
        .split(['/', ':', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('.');
    let labels = authority.split('.').collect::<Vec<_>>();
    if labels.len() == 4
        && labels.iter().all(|label| {
            !label.is_empty() && label.len() <= 3 && label.chars().all(|c| c.is_ascii_digit())
        })
    {
        return true;
    }
    if labels.len() < 2 {
        return false;
    }
    let Some(top_level) = labels.last() else {
        return false;
    };
    (2..=63).contains(&top_level.len())
        && top_level
            .chars()
            .all(|character| character.is_ascii_alphabetic())
        && labels[..labels.len() - 1].iter().all(|label| {
            !label.is_empty()
                && label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct RecordingLlm {
        result: Result<Option<String>, DuelFlavorPortError>,
        requests: Vec<DuelFlavorCompletionRequest>,
    }

    impl RecordingLlm {
        fn returning(value: Option<&str>) -> Self {
            Self {
                result: Ok(value.map(str::to_owned)),
                requests: Vec::new(),
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                result: Err(DuelFlavorPortError::new(message)),
                requests: Vec::new(),
            }
        }
    }

    impl DuelFlavorLlmPort for RecordingLlm {
        fn complete(
            &mut self,
            request: DuelFlavorCompletionRequest,
        ) -> Result<Option<String>, DuelFlavorPortError> {
            self.requests.push(request);
            self.result.clone()
        }
    }

    #[derive(Clone, Debug)]
    struct RecordingConfig {
        result: Result<bool, DuelFlavorPortError>,
        guild_ids: Vec<i64>,
    }

    impl RecordingConfig {
        fn returning(enabled: bool) -> Self {
            Self {
                result: Ok(enabled),
                guild_ids: Vec::new(),
            }
        }
    }

    impl DuelFlavorConfigPort for RecordingConfig {
        fn ai_enabled(&mut self, guild_id: i64) -> Result<bool, DuelFlavorPortError> {
            self.guild_ids.push(guild_id);
            self.result.clone()
        }
    }

    fn service_with_result(
        result: Option<&str>,
    ) -> DuelFlavorService<RecordingLlm, RecordingConfig, FirstDuelFlavorChoice> {
        DuelFlavorService::new(
            Some(RecordingLlm::returning(result)),
            Some(RecordingConfig::returning(true)),
            FirstDuelFlavorChoice,
        )
    }

    fn assert_unsafe_output_falls_back(unsafe_output: &str) {
        let mut service = service_with_result(Some(unsafe_output));
        let result = service.generate(FlavorEvent::Issued, 42, &[]);
        assert_eq!(result, fallbacks(FlavorEvent::Issued)[0]);
    }

    #[test]
    fn test_flavor_uses_dedicated_prompt_and_sanitizes_output() {
        let mut service = service_with_result(Some(
            "@everyone The court trembles.\n@here Roshan approves.",
        ));
        let result = service.generate(
            FlavorEvent::Issued,
            42,
            &[
                DuelFlavorDetail::new("challenger", "A<@1>"),
                DuelFlavorDetail::new("recipient", "B"),
                DuelFlavorDetail::new("wager", 500),
            ],
        );

        assert_eq!(
            result,
            "＠everyone The court trembles. ＠here Roshan approves."
        );
        assert!(!result.contains("@everyone"));
        assert!(!result.contains("@here"));
        assert!(!result.contains('@'));
        assert!(!result.contains('\n'));
        assert!(result.chars().count() <= 300);

        let requests = &service.llm().expect("LLM configured").requests;
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(!request.system_prompt.contains("Game of Thrones"));
        assert!(request.system_prompt.contains("Tales of Dunk and Egg"));
        assert!(request.system_prompt.contains("Do not quote or imitate"));
        assert_eq!(
            request.system_prompt,
            format!(
                "{} {} {}",
                SYSTEM_PROMPT, HERALD_VOICES[0], PROMPT_CONSTRAINTS
            )
        );
        assert!(request.system_prompt.ends_with(PROMPT_CONSTRAINTS));
        assert_eq!(request.feature, "duel.issued");
        assert_eq!(service.config().expect("config present").guild_ids, [42]);
    }

    #[test]
    fn test_flavor_falls_back_when_output_contains_link_or_markup_https_url() {
        assert_unsafe_output_falls_back("Meet me at https://example.com for the tilt.");
    }

    #[test]
    fn test_flavor_falls_back_when_output_contains_link_or_markup_bare_domain() {
        assert_unsafe_output_falls_back("The odds live at example.com/duel today.");
    }

    #[test]
    fn test_flavor_falls_back_when_output_contains_link_or_markup_ipv4() {
        assert_unsafe_output_falls_back("Petition the marshal at 192.168.1.1:8080 now.");
    }

    #[test]
    fn test_flavor_falls_back_when_output_contains_link_or_markup_markdown_link() {
        assert_unsafe_output_falls_back("[the rolls](https://evil.example) record it all.");
    }

    #[test]
    fn test_flavor_falls_back_when_output_contains_link_or_markup_discord_role() {
        assert_unsafe_output_falls_back("<@&123> the herald summons the banners.");
    }

    #[test]
    fn test_flavor_falls_back_when_output_contains_link_or_markup_discord_timestamp() {
        assert_unsafe_output_falls_back("Use the rune timer <t:1700000000> wisely.");
    }

    #[test]
    fn test_flavor_falls_back_when_ai_is_missing() {
        let mut service =
            DuelFlavorService::<RecordingLlm, RecordingConfig, FirstDuelFlavorChoice>::new(
                None,
                None,
                FirstDuelFlavorChoice,
            );

        let result = service.generate(FlavorEvent::Expired, 42, &[]);

        assert!(!result.is_empty());
        assert!(result.chars().count() <= 300);
        assert_eq!(result, fallbacks(FlavorEvent::Expired)[0]);
    }

    #[test]
    fn test_flavor_falls_back_without_calling_ai_when_guild_ai_is_disabled() {
        let mut service = DuelFlavorService::new(
            Some(RecordingLlm::returning(Some("unused"))),
            Some(RecordingConfig::returning(false)),
            FirstDuelFlavorChoice,
        );

        let result = service.generate(FlavorEvent::Declined, 42, &[]);

        assert_eq!(result, fallbacks(FlavorEvent::Declined)[0]);
        assert!(service.llm().expect("LLM configured").requests.is_empty());
        assert_eq!(service.config().expect("config present").guild_ids, [42]);
    }

    #[test]
    fn test_flavor_falls_back_when_provider_returns_no_usable_text_none() {
        let mut service = service_with_result(None);

        let result = service.generate(FlavorEvent::Voided, 42, &[]);

        assert_eq!(result, fallbacks(FlavorEvent::Voided)[0]);
    }

    #[test]
    fn test_flavor_falls_back_when_provider_returns_no_usable_text_whitespace() {
        let mut service = service_with_result(Some(" \n\t\r "));

        let result = service.generate(FlavorEvent::Voided, 42, &[]);

        assert_eq!(result, fallbacks(FlavorEvent::Voided)[0]);
    }

    #[test]
    fn test_flavor_falls_back_when_provider_raises() {
        let mut service = DuelFlavorService::new(
            Some(RecordingLlm::failing("provider unavailable")),
            Some(RecordingConfig::returning(true)),
            FirstDuelFlavorChoice,
        );

        let result = service.generate(FlavorEvent::Resolved, 42, &[]);

        assert_eq!(result, fallbacks(FlavorEvent::Resolved)[0]);
        assert_eq!(service.llm().expect("LLM configured").requests.len(), 1);
    }

    #[test]
    fn test_flavor_sanitizes_and_caps_details_before_prompt_interpolation() {
        let mut service = service_with_result(Some("The lists are opened."));
        let dangerous_name = format!("A<@1>`\n\r\t{}SECRET", "x".repeat(100));

        let result = service.generate(
            FlavorEvent::Reminder,
            42,
            &[DuelFlavorDetail::new("challenger", dangerous_name)],
        );

        assert_eq!(result, "The lists are opened.");
        let prompt = &service.llm().expect("LLM configured").requests[0].prompt;
        assert!(
            !prompt.chars().any(|character| {
                matches!(character, '<' | '>' | '@' | '`' | '\n' | '\r' | '\t')
            })
        );
        assert!(!prompt.contains("SECRET"));
        assert!(prompt.contains(&"x".repeat(78)));
        assert!(!prompt.contains(&"x".repeat(79)));
    }

    #[test]
    fn test_every_lifecycle_event_has_a_distinct_fallback() {
        let mut service =
            DuelFlavorService::<RecordingLlm, RecordingConfig, FirstDuelFlavorChoice>::new(
                None,
                None,
                FirstDuelFlavorChoice,
            );

        let results = ALL_FLAVOR_EVENTS
            .iter()
            .copied()
            .map(|event| service.generate(event, 42, &[]))
            .collect::<Vec<_>>();

        assert_eq!(results.len(), ALL_FLAVOR_EVENTS.len());
        assert!(results.iter().all(|result| !result.is_empty()));
        assert!(results.iter().all(|result| result.chars().count() <= 300));
        for (index, result) in results.iter().enumerate() {
            assert!(!results[..index].contains(result));
        }
    }
}
