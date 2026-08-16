//! Discord-neutral JOPA-T post-match prompt and fallback policy.
//!
//! Python remains the live Discord and model-provider authority during the
//! side-by-side migration. This module owns only deterministic catalog,
//! selection, prompt, sanitization, and message-budget behavior. Runtime
//! entropy is injected through [`JopatChoicePort`].

use std::collections::BTreeMap;

use crate::neon_degen::ansi_block;

const FALLBACK_PAYLOAD_LIMIT: usize = 1_800;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JopatProtocol {
    CombatTelemetry,
    MarketSettlement,
    ClientAscension,
    PerformanceReview,
    PatchDeployment,
    ComplianceIncident,
    ChatIngest,
}

pub const JOPAT_PROTOCOLS: [JopatProtocol; 7] = [
    JopatProtocol::CombatTelemetry,
    JopatProtocol::MarketSettlement,
    JopatProtocol::ClientAscension,
    JopatProtocol::PerformanceReview,
    JopatProtocol::PatchDeployment,
    JopatProtocol::ComplianceIncident,
    JopatProtocol::ChatIngest,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NumericFact(String);

impl NumericFact {
    #[must_use]
    pub fn integer(value: i64) -> Self {
        Self(value.to_string())
    }

    #[must_use]
    pub fn float(value: f64) -> Self {
        let rendered = if value.is_finite() && value.fract() == 0.0 {
            format!("{value:.1}")
        } else {
            value.to_string()
        };
        Self(rendered)
    }

    /// Construct an arbitrarily large integer fact, matching Python's
    /// unbounded integer display without weakening the rest of the context.
    #[must_use]
    pub fn arbitrary_integer(value: &str) -> Option<Self> {
        let digits = value.strip_prefix('-').unwrap_or(value);
        (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn as_f64(&self) -> Option<f64> {
        self.0.parse().ok()
    }

    fn bounded_display(&self) -> String {
        if self.0.chars().count() <= 32 {
            return self.0.clone();
        }
        format!("{}...", self.0.chars().take(29).collect::<String>())
    }
}

impl From<i64> for NumericFact {
    fn from(value: i64) -> Self {
        Self::integer(value)
    }
}

impl From<f64> for NumericFact {
    fn from(value: f64) -> Self {
        Self::float(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct JopatPostMatchContext {
    pub winner_name: Option<String>,
    pub loser_name: Option<String>,
    pub hero: Option<String>,
    pub kills: Option<NumericFact>,
    pub deaths: Option<NumericFact>,
    pub assists: Option<NumericFact>,
    pub gpm: Option<NumericFact>,
    pub xpm: Option<NumericFact>,
    pub rating_change: Option<NumericFact>,
    pub expected_win_prob: Option<NumericFact>,
    pub payout: i64,
    pub loss: i64,
    pub leverage: i64,
    pub bankruptcy_count: i64,
    pub degen_score: Option<NumericFact>,
    pub streak: i64,
}

impl Default for JopatPostMatchContext {
    fn default() -> Self {
        Self {
            winner_name: None,
            loser_name: None,
            hero: None,
            kills: None,
            deaths: None,
            assists: None,
            gpm: None,
            xpm: None,
            rating_change: None,
            expected_win_prob: None,
            payout: 0,
            loss: 0,
            leverage: 1,
            bankruptcy_count: 0,
            degen_score: None,
            streak: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PromptFact {
    Text(String),
    Number(NumericFact),
    Integer(i64),
}

impl JopatPostMatchContext {
    /// Return only supplied facts and non-neutral scalar defaults.
    #[must_use]
    pub fn prompt_context(&self) -> BTreeMap<&'static str, PromptFact> {
        let mut facts = BTreeMap::new();
        insert_text(&mut facts, "winner_name", self.winner_name.as_deref());
        insert_text(&mut facts, "loser_name", self.loser_name.as_deref());
        insert_text(&mut facts, "hero", self.hero.as_deref());
        insert_number(&mut facts, "kills", self.kills.as_ref());
        insert_number(&mut facts, "deaths", self.deaths.as_ref());
        insert_number(&mut facts, "assists", self.assists.as_ref());
        insert_number(&mut facts, "gpm", self.gpm.as_ref());
        insert_number(&mut facts, "xpm", self.xpm.as_ref());
        insert_number(&mut facts, "rating_change", self.rating_change.as_ref());
        insert_number(
            &mut facts,
            "expected_win_prob",
            self.expected_win_prob.as_ref(),
        );
        insert_number(&mut facts, "degen_score", self.degen_score.as_ref());
        insert_non_neutral(&mut facts, "payout", self.payout, 0);
        insert_non_neutral(&mut facts, "loss", self.loss, 0);
        insert_non_neutral(&mut facts, "leverage", self.leverage, 1);
        insert_non_neutral(&mut facts, "bankruptcy_count", self.bankruptcy_count, 0);
        insert_non_neutral(&mut facts, "streak", self.streak, 0);
        facts
    }
}

fn insert_text(
    facts: &mut BTreeMap<&'static str, PromptFact>,
    key: &'static str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        facts.insert(key, PromptFact::Text(value.to_owned()));
    }
}

fn insert_number(
    facts: &mut BTreeMap<&'static str, PromptFact>,
    key: &'static str,
    value: Option<&NumericFact>,
) {
    if let Some(value) = value {
        facts.insert(key, PromptFact::Number(value.clone()));
    }
}

fn insert_non_neutral(
    facts: &mut BTreeMap<&'static str, PromptFact>,
    key: &'static str,
    value: i64,
    neutral: i64,
) {
    if value != neutral {
        facts.insert(key, PromptFact::Integer(value));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactKey {
    WinnerName,
    LoserName,
    Hero,
    Kills,
    Deaths,
    Assists,
    Gpm,
    Xpm,
    RatingChange,
    ExpectedWinProb,
    Payout,
    Loss,
    Leverage,
    BankruptcyCount,
    DegenScore,
    Streak,
}

impl FactKey {
    const fn name(self) -> &'static str {
        match self {
            Self::WinnerName => "winner_name",
            Self::LoserName => "loser_name",
            Self::Hero => "hero",
            Self::Kills => "kills",
            Self::Deaths => "deaths",
            Self::Assists => "assists",
            Self::Gpm => "gpm",
            Self::Xpm => "xpm",
            Self::RatingChange => "rating_change",
            Self::ExpectedWinProb => "expected_win_prob",
            Self::Payout => "payout",
            Self::Loss => "loss",
            Self::Leverage => "leverage",
            Self::BankruptcyCount => "bankruptcy_count",
            Self::DegenScore => "degen_score",
            Self::Streak => "streak",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FallbackTemplate {
    pub text: &'static str,
    pub required: &'static [FactKey],
}

const fn template(text: &'static str, required: &'static [FactKey]) -> FallbackTemplate {
    FallbackTemplate { text, required }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JopatProtocolSpec {
    pub heading: &'static str,
    pub instruction: &'static str,
    pub hype_fallbacks: &'static [FallbackTemplate],
    pub roast_fallbacks: &'static [FallbackTemplate],
    pub neutral_fallbacks: &'static [FallbackTemplate],
}

impl JopatProtocolSpec {
    #[must_use]
    pub fn fallbacks(self) -> Vec<&'static str> {
        self.hype_fallbacks
            .iter()
            .chain(self.roast_fallbacks)
            .chain(self.neutral_fallbacks)
            .map(|fallback| fallback.text)
            .collect()
    }
}

const COMBAT_HYPE: &[FallbackTemplate] = &[
    template(
        "Combat output accepted. Hero {hero} has been cleared of responsibility pending replay review.",
        &[FactKey::Hero],
    ),
    template(
        "K/D/A {kills}/{deaths}/{assists} approved. The replay parser has upgraded skepticism to applause.",
        &[FactKey::Kills, FactKey::Deaths, FactKey::Assists],
    ),
];
const COMBAT_ROAST: &[FallbackTemplate] = &[
    template(
        "Hero: {hero} | K/D/A: {kills}/{deaths}/{assists}\nEfficiency review: courage exceeded accuracy.",
        &[
            FactKey::Hero,
            FactKey::Kills,
            FactKey::Deaths,
            FactKey::Assists,
        ],
    ),
    template(
        "Combat sample archived. {deaths} deaths produced {assists} assists. Conversion remains theoretical.",
        &[FactKey::Deaths, FactKey::Assists],
    ),
    template(
        "Hero telemetry: {hero}. GPM {gpm}, XPM {xpm}. The map filed a noise complaint.",
        &[FactKey::Hero, FactKey::Gpm, FactKey::Xpm],
    ),
    template(
        "Client {loser_name} completed the match with {kills} kills. Completion is the strongest available metric.",
        &[FactKey::LoserName, FactKey::Kills],
    ),
    template(
        "K/D/A received: {kills}/{deaths}/{assists}. The slash marks are doing structural work.",
        &[FactKey::Kills, FactKey::Deaths, FactKey::Assists],
    ),
];
const COMBAT_NEUTRAL: &[FallbackTemplate] = &[template(
    "Combat sample logged. Conclusions remain queued behind the replay parser.",
    &[],
)];

const MARKET_HYPE: &[FallbackTemplate] = &[
    template(
        "Settlement posted. Payout: {payout} JC. The client briefly outperformed the house spreadsheet.",
        &[FactKey::Payout],
    ),
    template(
        "Payout {payout} JC approved. Future donations to the market remain anticipated.",
        &[FactKey::Payout],
    ),
];
const MARKET_ROAST: &[FallbackTemplate] = &[
    template(
        "Ledger closed. Loss: {loss} JC. Confidence remains the most expensive position.",
        &[FactKey::Loss],
    ),
    template(
        "Loss {loss} JC recorded. The wager had conviction; evidence was unavailable.",
        &[FactKey::Loss],
    ),
    template(
        "Account settled. The market thanks the client for confusing volatility with a plan.",
        &[],
    ),
];
const MARKET_NEUTRAL: &[FallbackTemplate] = &[template(
    "Exposure: {leverage}x. Result processed without congratulating the risk model.",
    &[FactKey::Leverage],
)];

const ASCENSION_HYPE: &[FallbackTemplate] = &[
    template(
        "Rating change: {rating_change}. Promotion paperwork opened; permanence not implied.",
        &[FactKey::RatingChange],
    ),
    template(
        "Underdog probability: {expected_win_prob}. Forecast rejected by one inconvenient result.",
        &[FactKey::ExpectedWinProb],
    ),
    template(
        "Win streak: {streak}. The correction model has been notified.",
        &[FactKey::Streak],
    ),
    template(
        "Client {winner_name} ascended. Ceiling inspection is now scheduled.",
        &[FactKey::WinnerName],
    ),
    template(
        "Performance tier increased. Sample size remains the department's preferred objection.",
        &[],
    ),
    template(
        "Success confirmed. JOPA-T has provisionally upgraded the client from liability to anomaly.",
        &[],
    ),
];
const EMPTY_FALLBACKS: &[FallbackTemplate] = &[];

const PERFORMANCE_HYPE: &[FallbackTemplate] = &[
    template(
        "Client {winner_name} delivered a result. Management requests fewer dramatic intermediate steps.",
        &[FactKey::WinnerName],
    ),
    template(
        "Review approved. Objective conversion exceeded the department's preferred forecast.",
        &[],
    ),
];
const PERFORMANCE_ROAST: &[FallbackTemplate] = &[
    template(
        "Employee: {loser_name} | Hero: {hero}\nReview outcome: further supervision recommended.",
        &[FactKey::LoserName, FactKey::Hero],
    ),
    template(
        "K/D/A {kills}/{deaths}/{assists}. Meets attendance expectations; misses several others.",
        &[FactKey::Kills, FactKey::Deaths, FactKey::Assists],
    ),
    template(
        "GPM {gpm}, XPM {xpm}. Resource acquisition did not become resource usefulness.",
        &[FactKey::Gpm, FactKey::Xpm],
    ),
    template(
        "Review filed. Core competency: making routine objectives look like emergency projects.",
        &[],
    ),
    template(
        "Performance accepted at the minimum viable standard. The bar has requested relocation.",
        &[],
    ),
];
const PERFORMANCE_NEUTRAL: &[FallbackTemplate] = &[template(
    "Performance sample archived. Replay conclusions remain pending.",
    &[],
)];

const PATCH_HYPE: &[FallbackTemplate] = &[
    template(
        "Build notes: {kills} kills, {assists} assists. Team compatibility passed field testing.",
        &[FactKey::Kills, FactKey::Assists],
    ),
    template(
        "Patch deployed. Decision latency improved from eventually to slightly earlier.",
        &[],
    ),
];
const PATCH_ROAST: &[FallbackTemplate] = &[
    template(
        "Patch target: {hero}. Reduced confidence scaling after {deaths} field failures.",
        &[FactKey::Hero, FactKey::Deaths],
    ),
    template(
        "Hotfix queued: convert {gpm} GPM into one timely objective.",
        &[FactKey::Gpm],
    ),
    template(
        "Client update staged. Removed one excuse; known issue list unchanged.",
        &[],
    ),
    template(
        "Version review complete. Gameplay remains backward-compatible with disappointment.",
        &[],
    ),
];
const PATCH_NEUTRAL: &[FallbackTemplate] = &[template(
    "Build archived. Compatibility conclusions remain with the replay parser.",
    &[],
)];

const COMPLIANCE_HYPE: &[FallbackTemplate] = &[template(
    "Payout {payout} JC triggered enhanced monitoring for sudden strategic confidence.",
    &[FactKey::Payout],
)];
const COMPLIANCE_ROAST: &[FallbackTemplate] = &[
    template(
        "Incident: {leverage}x exposure. Risk controls were present and respectfully ignored.",
        &[FactKey::Leverage],
    ),
    template(
        "Bankruptcy filings: {bankruptcy_count}. Client onboarding has become a recurring ceremony.",
        &[FactKey::BankruptcyCount],
    ),
    template(
        "Degen score: {degen_score}. Compliance recommends a strategy; client recommends another wager.",
        &[FactKey::DegenScore],
    ),
    template(
        "Loss {loss} JC logged. Internal controls confirm the button worked exactly as pressed.",
        &[FactKey::Loss],
    ),
];
const COMPLIANCE_NEUTRAL: &[FallbackTemplate] = &[template(
    "Compliance review closed. No rules broken; several lessons declined.",
    &[],
)];

const CHAT_HYPE: &[FallbackTemplate] = &[template(
    "Client {winner_name} has entered chat. Result-based expertise detected.",
    &[FactKey::WinnerName],
)];
const CHAT_ROAST: &[FallbackTemplate] = &[
    template(
        "Chat signal: {hero} was 'online.' Telemetry has submitted a clarification.",
        &[FactKey::Hero],
    ),
    template(
        "Claim received: {kills} kills. Counterpoint received: {deaths} deaths.",
        &[FactKey::Kills, FactKey::Deaths],
    ),
    template(
        "Post-match confidence exceeds {gpm} GPM. Analytics is investigating the spread.",
        &[FactKey::Gpm],
    ),
    template(
        "Client {loser_name} has supplied feedback. Replay evidence remains read-only.",
        &[FactKey::LoserName],
    ),
];
const CHAT_NEUTRAL: &[FallbackTemplate] = &[template(
    "Simulated chat ingest complete. Actionable information remains below threshold.",
    &[],
)];

const COMBAT_SPEC: JopatProtocolSpec = JopatProtocolSpec {
    heading: "COMBAT TELEMETRY",
    instruction: "Issue a terse combat audit in JOPA-T's deadpan corporate voice; judge only the reported gameplay metrics.",
    hype_fallbacks: COMBAT_HYPE,
    roast_fallbacks: COMBAT_ROAST,
    neutral_fallbacks: COMBAT_NEUTRAL,
};
const MARKET_SPEC: JopatProtocolSpec = JopatProtocolSpec {
    heading: "MARKET SETTLEMENT",
    instruction: "Deliver a compact settlement notice in JOPA-T's corporate-terminal voice; mock only the reported wager result.",
    hype_fallbacks: MARKET_HYPE,
    roast_fallbacks: MARKET_ROAST,
    neutral_fallbacks: MARKET_NEUTRAL,
};
const ASCENSION_SPEC: JopatProtocolSpec = JopatProtocolSpec {
    heading: "CLIENT ASCENSION",
    instruction: "Announce a narrowly verified rise in status in JOPA-T's dry corporate voice; treat success as a temporary anomaly.",
    hype_fallbacks: ASCENSION_HYPE,
    roast_fallbacks: EMPTY_FALLBACKS,
    neutral_fallbacks: EMPTY_FALLBACKS,
};
const PERFORMANCE_SPEC: JopatProtocolSpec = JopatProtocolSpec {
    heading: "PERFORMANCE REVIEW",
    instruction: "Write a short performance review in JOPA-T's unified corporate voice; criticize only supplied match performance.",
    hype_fallbacks: PERFORMANCE_HYPE,
    roast_fallbacks: PERFORMANCE_ROAST,
    neutral_fallbacks: PERFORMANCE_NEUTRAL,
};
const PATCH_SPEC: JopatProtocolSpec = JopatProtocolSpec {
    heading: "PATCH DEPLOYMENT",
    instruction: "Frame the reported gameplay as a compact software patch note in JOPA-T's deadpan terminal voice.",
    hype_fallbacks: PATCH_HYPE,
    roast_fallbacks: PATCH_ROAST,
    neutral_fallbacks: PATCH_NEUTRAL,
};
const COMPLIANCE_SPEC: JopatProtocolSpec = JopatProtocolSpec {
    heading: "COMPLIANCE INCIDENT",
    instruction: "Issue a brief compliance incident report in JOPA-T's corporate gambling-terminal voice; cite only supplied betting behavior.",
    hype_fallbacks: COMPLIANCE_HYPE,
    roast_fallbacks: COMPLIANCE_ROAST,
    neutral_fallbacks: COMPLIANCE_NEUTRAL,
};
const CHAT_SPEC: JopatProtocolSpec = JopatProtocolSpec {
    heading: "CHAT INGEST",
    instruction: "Summarize the match as a terse chat-ingest finding in JOPA-T's single corporate identity; roast only gameplay claims.",
    hype_fallbacks: CHAT_HYPE,
    roast_fallbacks: CHAT_ROAST,
    neutral_fallbacks: CHAT_NEUTRAL,
};

#[must_use]
pub const fn protocol_spec(protocol: JopatProtocol) -> &'static JopatProtocolSpec {
    match protocol {
        JopatProtocol::CombatTelemetry => &COMBAT_SPEC,
        JopatProtocol::MarketSettlement => &MARKET_SPEC,
        JopatProtocol::ClientAscension => &ASCENSION_SPEC,
        JopatProtocol::PerformanceReview => &PERFORMANCE_SPEC,
        JopatProtocol::PatchDeployment => &PATCH_SPEC,
        JopatProtocol::ComplianceIncident => &COMPLIANCE_SPEC,
        JopatProtocol::ChatIngest => &CHAT_SPEC,
    }
}

pub trait JopatChoicePort {
    fn choose_index(&mut self, option_count: usize) -> usize;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeededJopatChoice {
    state: u64,
}

impl SeededJopatChoice {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }
}

impl JopatChoicePort for SeededJopatChoice {
    fn choose_index(&mut self, option_count: usize) -> usize {
        if option_count == 0 {
            return 0;
        }
        let index = usize::try_from(self.state % option_count as u64).unwrap_or(0);
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        index
    }
}

#[must_use]
pub fn protocol_candidates(context: &JopatPostMatchContext) -> Vec<JopatProtocol> {
    let mut candidates = Vec::new();
    let has_market_context = context.payout != 0
        || context.loss != 0
        || context.leverage != 1
        || context.bankruptcy_count != 0
        || context.degen_score.is_some();
    if has_market_context {
        candidates.extend([
            JopatProtocol::MarketSettlement,
            JopatProtocol::ComplianceIncident,
        ]);
    }

    let has_gameplay_context = context.hero.is_some()
        || context.kills.is_some()
        || context.deaths.is_some()
        || context.assists.is_some()
        || context.gpm.is_some()
        || context.xpm.is_some();
    if has_gameplay_context {
        candidates.extend([
            JopatProtocol::CombatTelemetry,
            JopatProtocol::PerformanceReview,
            JopatProtocol::PatchDeployment,
            JopatProtocol::ChatIngest,
        ]);
    }

    let positive_rating = context
        .rating_change
        .as_ref()
        .and_then(NumericFact::as_f64)
        .is_some_and(|value| value > 0.0);
    let underdog_winner = context.winner_name.is_some()
        && context
            .expected_win_prob
            .as_ref()
            .and_then(NumericFact::as_f64)
            .is_some_and(|value| value < 0.5);
    if positive_rating || underdog_winner || context.streak > 0 {
        candidates.push(JopatProtocol::ClientAscension);
    }

    if candidates.is_empty() {
        candidates.extend([
            JopatProtocol::PerformanceReview,
            JopatProtocol::PatchDeployment,
            JopatProtocol::ChatIngest,
        ]);
    }
    candidates
}

pub fn choose_protocol<C>(context: &JopatPostMatchContext, choice: &mut C) -> JopatProtocol
where
    C: JopatChoicePort,
{
    let candidates = protocol_candidates(context);
    candidates[choice.choose_index(candidates.len()) % candidates.len()]
}

#[must_use]
pub fn build_event_description(protocol: JopatProtocol, context: &JopatPostMatchContext) -> String {
    let spec = protocol_spec(protocol);
    let mut facts = context.prompt_context();
    redact_prompt_name(&mut facts, "winner_name", "[verified winner client]");
    redact_prompt_name(&mut facts, "loser_name", "[verified loser client]");
    format!(
        "Protocol: {}\nInstruction: {}\nKnown facts: {}\nUse only these known facts. Do not invent heroes, stats, or outcomes.",
        spec.heading,
        spec.instruction,
        serialize_prompt_facts(&facts)
    )
}

fn redact_prompt_name(
    facts: &mut BTreeMap<&'static str, PromptFact>,
    key: &'static str,
    replacement: &str,
) {
    if matches!(facts.get(key), Some(PromptFact::Text(value)) if !value.is_empty()) {
        facts.insert(key, PromptFact::Text(replacement.to_owned()));
    }
}

fn serialize_prompt_facts(facts: &BTreeMap<&str, PromptFact>) -> String {
    let entries = facts
        .iter()
        .map(|(key, value)| {
            let value = match value {
                PromptFact::Text(value) => format!("\"{}\"", json_escape_ascii(value)),
                PromptFact::Number(value) => value.as_str().to_owned(),
                PromptFact::Integer(value) => value.to_string(),
            };
            format!("\"{}\": {value}", json_escape_ascii(key))
        })
        .collect::<Vec<_>>();
    format!("{{{}}}", entries.join(", "))
}

fn json_escape_ascii(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\u{8}' => escaped.push_str("\\b"),
            '\u{c}' => escaped.push_str("\\f"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if (' '..='~').contains(&character) => escaped.push(character),
            character => push_json_unicode_escape(&mut escaped, character),
        }
    }
    escaped
}

fn push_json_unicode_escape(output: &mut String, character: char) {
    let scalar = u32::from(character);
    if scalar <= 0xffff {
        output.push_str(&format!("\\u{scalar:04x}"));
        return;
    }
    let offset = scalar - 0x1_0000;
    let high = 0xd800 + (offset >> 10);
    let low = 0xdc00 + (offset & 0x3ff);
    output.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
}

pub fn render_fallback<C>(
    protocol: JopatProtocol,
    context: &JopatPostMatchContext,
    choice: &mut C,
) -> String
where
    C: JopatChoicePort,
{
    let spec = protocol_spec(protocol);
    let prompt_facts = context.prompt_context();
    if prompt_facts.is_empty() {
        return render_bounded_ansi(&format!(
            "[JOPA-T] {}\n{}",
            spec.heading,
            missing_context_fallback(protocol)
        ));
    }

    let facts = safe_fallback_facts(&prompt_facts);
    let templates = fallback_pool(spec, context)
        .into_iter()
        .filter(|fallback| {
            fallback
                .required
                .iter()
                .all(|key| facts.contains_key(key.name()))
        })
        .collect::<Vec<_>>();
    if templates.is_empty() {
        return render_bounded_ansi(&format!(
            "[JOPA-T] {}\n{}",
            spec.heading,
            missing_context_fallback(protocol)
        ));
    }

    let selected = templates[choice.choose_index(templates.len()) % templates.len()];
    let body = render_template(selected.text, &facts);
    render_bounded_ansi(&format!("[JOPA-T] {}\n{body}", spec.heading))
}

fn fallback_pool(
    spec: &JopatProtocolSpec,
    context: &JopatPostMatchContext,
) -> Vec<&'static FallbackTemplate> {
    let is_winner = context
        .winner_name
        .as_ref()
        .is_some_and(|name| !name.is_empty())
        || context.payout > 0
        || context
            .rating_change
            .as_ref()
            .and_then(NumericFact::as_f64)
            .is_some_and(|value| value > 0.0)
        || context.streak > 0;
    let is_loser = context
        .loser_name
        .as_ref()
        .is_some_and(|name| !name.is_empty())
        || context.loss > 0
        || context
            .rating_change
            .as_ref()
            .and_then(NumericFact::as_f64)
            .is_some_and(|value| value < 0.0)
        || context.streak < 0;
    if is_winner && !is_loser {
        return spec
            .hype_fallbacks
            .iter()
            .chain(spec.neutral_fallbacks)
            .collect();
    }
    if is_loser && !is_winner {
        return spec
            .roast_fallbacks
            .iter()
            .chain(spec.neutral_fallbacks)
            .collect();
    }
    spec.hype_fallbacks
        .iter()
        .chain(spec.roast_fallbacks)
        .chain(spec.neutral_fallbacks)
        .collect()
}

fn safe_fallback_facts(
    facts: &BTreeMap<&'static str, PromptFact>,
) -> BTreeMap<&'static str, String> {
    facts
        .iter()
        .map(|(key, value)| {
            let value = match value {
                PromptFact::Text(value) => sanitize_terminal_label(value),
                PromptFact::Number(value) => value.bounded_display(),
                PromptFact::Integer(value) => {
                    let rendered = value.to_string();
                    if rendered.len() > 32 {
                        format!("{}...", rendered.chars().take(29).collect::<String>())
                    } else {
                        rendered
                    }
                }
            };
            (*key, value)
        })
        .collect()
}

fn sanitize_terminal_label(value: &str) -> String {
    let folded = value.chars().map(compatibility_fold).collect::<String>();
    let mut safe = String::new();
    let mut characters = folded.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\u{1b}' if characters.peek() == Some(&'[') => {
                characters.next();
                skip_control_sequence(&mut characters);
            }
            '\u{1b}' if characters.peek() == Some(&']') => {
                characters.next();
                skip_osc_sequence(&mut characters);
            }
            '\u{9b}' => skip_control_sequence(&mut characters),
            '\u{9d}' => skip_osc_sequence(&mut characters),
            '`' => safe.push('\''),
            character if character.is_control() || is_unicode_format(character) => {}
            character => safe.push(character),
        }
    }
    safe.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
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

fn skip_control_sequence(characters: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for character in characters.by_ref() {
        if ('@'..='~').contains(&character) {
            break;
        }
    }
}

fn skip_osc_sequence(characters: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(character) = characters.next() {
        if character == '\u{7}' {
            break;
        }
        if character == '\u{1b}' && characters.peek() == Some(&'\\') {
            characters.next();
            break;
        }
        if character == '\u{9c}' {
            break;
        }
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

fn render_template(template: &str, facts: &BTreeMap<&str, String>) -> String {
    facts
        .iter()
        .fold(template.to_owned(), |rendered, (key, value)| {
            rendered.replace(&format!("{{{key}}}"), value)
        })
}

fn render_bounded_ansi(payload: &str) -> String {
    let bounded = if payload.chars().count() > FALLBACK_PAYLOAD_LIMIT {
        let prefix = payload
            .chars()
            .take(FALLBACK_PAYLOAD_LIMIT - 3)
            .collect::<String>();
        format!("{}...", prefix.trim_end())
    } else {
        payload.to_owned()
    };
    ansi_block(&bounded)
}

const fn missing_context_fallback(protocol: JopatProtocol) -> &'static str {
    match protocol {
        JopatProtocol::CombatTelemetry => "No verified combat telemetry was supplied.",
        JopatProtocol::MarketSettlement => "No verified settlement facts were supplied.",
        JopatProtocol::ClientAscension => "No verified upward movement was supplied.",
        JopatProtocol::PerformanceReview => "No verified performance telemetry was supplied.",
        JopatProtocol::PatchDeployment => "No verified gameplay delta was supplied.",
        JopatProtocol::ComplianceIncident => "No verified wager facts were supplied.",
        JopatProtocol::ChatIngest => "No verified chat or gameplay facts were supplied.",
    }
}

#[cfg(test)]
#[path = "jopat_post_match/tests.rs"]
mod tests;
