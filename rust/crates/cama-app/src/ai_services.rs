//! Typed, fail-soft AI orchestration for the side-by-side Rust migration.
//!
//! Python remains the live provider and SQLite authority.  This module ports
//! the policy at its seams: credentials stay redacted, providers are loaded on
//! first use off the caller thread, every attempt has an independent timeout,
//! optional fallback providers are tried in order, telemetry never receives
//! prompt bodies, generated SQL is structurally validated, and flavor output
//! is sanitized before display.  Provider SDKs and a live SQLite adapter are
//! deliberately ports so the tranche has no network or database side effects.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error as StdError;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, LazyLock, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

pub const SQL_TOOL_NAME: &str = "execute_sql_query";
pub const FLAVOR_TOOL_NAME: &str = "generate_flavor_text";

/// Match the noisy LiteLLM/Pydantic serializer warning suppressed by Python.
#[must_use]
pub fn is_suppressed_provider_warning(message: &str) -> bool {
    message.starts_with("Pydantic serializer warnings:")
        && message.contains("PydanticSerializationUnexpectedValue")
        && message.contains("Expected 10 fields but got 5")
        && message.contains("Expected `Message`")
}

/// Secret provider material.  Debug and Display intentionally reveal nothing.
#[derive(Clone, Eq, PartialEq)]
pub struct ProviderCredential(String);

impl ProviderCredential {
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        if value.trim().is_empty() || value.chars().any(char::is_control) {
            return Err(ConfigError::InvalidCredential);
        }
        Ok(Self(value))
    }

    /// Available only to provider adapters; callers should never log it.
    #[must_use]
    pub fn expose_to_provider(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ProviderCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderCredential([REDACTED])")
    }
}

impl fmt::Display for ProviderCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigError {
    EmptyModel,
    InvalidCredential,
    ZeroTimeout,
    ZeroMaxTokens,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyModel => "provider model must not be empty",
            Self::InvalidCredential => "provider credential must be non-empty and single-line",
            Self::ZeroTimeout => "provider timeout must be greater than zero",
            Self::ZeroMaxTokens => "provider max_tokens must be greater than zero",
        };
        formatter.write_str(message)
    }
}

impl StdError for ConfigError {}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ProviderName {
    Groq,
    Cerebras,
    Other(String),
    Unknown,
}

impl ProviderName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Groq => "groq",
            Self::Cerebras => "cerebras",
            Self::Other(name) => name,
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderConfig {
    pub model: String,
    pub provider: ProviderName,
    pub provider_model: String,
    pub credential: ProviderCredential,
    pub timeout: Duration,
    pub max_tokens: u32,
}

impl ProviderConfig {
    pub fn new(
        model: impl Into<String>,
        credential: ProviderCredential,
        timeout: Duration,
        max_tokens: u32,
    ) -> Result<Self, ConfigError> {
        let model = model.into();
        if model.trim().is_empty() {
            return Err(ConfigError::EmptyModel);
        }
        if timeout.is_zero() {
            return Err(ConfigError::ZeroTimeout);
        }
        if max_tokens == 0 {
            return Err(ConfigError::ZeroMaxTokens);
        }
        let (provider, provider_model) = model.split_once('/').map_or_else(
            || (ProviderName::Unknown, model.clone()),
            |(provider, provider_model)| {
                let provider = match provider.to_ascii_lowercase().as_str() {
                    "groq" => ProviderName::Groq,
                    "cerebras" => ProviderName::Cerebras,
                    other => ProviderName::Other(other.to_owned()),
                };
                (provider, provider_model.to_owned())
            },
        );
        Ok(Self {
            model,
            provider,
            provider_model,
            credential,
            timeout,
            max_tokens,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
}

impl Message {
    #[must_use]
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    Real(f64),
    Text(String),
    List(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl Value {
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    /// Read a JSON number without losing the integer case.  Tool payloads
    /// arrive from providers as either an integer or a real, while Dig's
    /// `flavor_bonus_pct` contract intentionally advertises the broader JSON
    /// `number` type.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer(value) => Some(*value as f64),
            Self::Real(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub properties: Vec<ToolProperty>,
    pub required: Vec<String>,
    pub additional_properties: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolProperty {
    pub name: String,
    pub description: String,
    pub enum_values: Vec<String>,
    pub schema: ToolPropertySchema,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ToolPropertySchema {
    #[default]
    String,
    /// A JSON number (integer or floating point).
    ///
    /// This intentionally has no range metadata yet.  The Python tool
    /// contracts that use it perform their bounds checking after the tool
    /// call (for example, `flavor_bonus_pct` is clamped to the per-dig cap),
    /// so adding an unadvertised range here would change provider behavior.
    Number,
    StringArray {
        min_items: usize,
        max_items: usize,
        unique_items: bool,
    },
    /// A nested JSON object with recursively typed properties.
    ///
    /// `required` is kept at this level because OpenAI-compatible JSON Schema
    /// places it beside `properties`, rather than on each child property.
    Object {
        properties: Vec<ToolProperty>,
        required: Vec<String>,
        additional_properties: Option<bool>,
    },
}

impl ToolDefinition {
    #[must_use]
    pub fn sql() -> Self {
        Self {
            name: SQL_TOOL_NAME.to_owned(),
            description: "Execute a focused SQL query. Select ONLY 1-3 columns that directly answer the question. Always include the player name and the key metric.".to_owned(),
            properties: vec![
                ToolProperty {
                    name: "sql".to_owned(),
                    description: "Minimal SELECT query with only essential columns. Example: SELECT discord_username, total_loans_taken FROM... (not SELECT *)".to_owned(),
                    enum_values: Vec::new(),
                    schema: ToolPropertySchema::String,
                },
                ToolProperty {
                    name: "explanation".to_owned(),
                    description: "One sentence explaining what this returns".to_owned(),
                    enum_values: Vec::new(),
                    schema: ToolPropertySchema::String,
                },
            ],
            required: vec!["sql".to_owned(), "explanation".to_owned()],
            additional_properties: None,
        }
    }

    #[must_use]
    pub fn flavor() -> Self {
        Self {
            name: FLAVOR_TOOL_NAME.to_owned(),
            description: "Generate a short, snarky comment about a player event".to_owned(),
            properties: vec![
                ToolProperty {
                    name: "comment".to_owned(),
                    description:
                        "1-2 sentence roast/comment. Be funny and reference the player's history."
                            .to_owned(),
                    enum_values: Vec::new(),
                    schema: ToolPropertySchema::String,
                },
                ToolProperty {
                    name: "tone".to_owned(),
                    description: "The tone of the comment".to_owned(),
                    enum_values: ["roast", "congratulations", "sympathy", "shock"]
                        .map(str::to_owned)
                        .to_vec(),
                    schema: ToolPropertySchema::String,
                },
            ],
            required: vec!["comment".to_owned()],
            additional_properties: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolChoice {
    Auto,
    None,
    Required(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProviderOptions {
    pub reasoning_format: Option<String>,
    pub reasoning_effort: Option<String>,
    pub allowed_openai_params: Vec<String>,
    pub parallel_tool_calls: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AiOperation {
    Completion,
    ToolCall,
}

impl AiOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completion => "completion",
            Self::ToolCall => "tool_call",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderRequest {
    pub operation: AiOperation,
    pub model: String,
    pub credential: ProviderCredential,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: ToolChoice,
    pub temperature: Option<f64>,
    pub max_tokens: u32,
    pub timeout: Duration,
    pub num_retries: u8,
    pub options: ProviderOptions,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub name: String,
    pub arguments: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TokenUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderResponse {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderErrorKind {
    RateLimit,
    Timeout,
    Authentication,
    Unavailable,
    InvalidResponse,
    Loader,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
}

impl ProviderError {
    #[must_use]
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl StdError for ProviderError {}

pub trait AiProvider: Send + Sync + 'static {
    fn complete(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError>;
}

pub trait ProviderLoader: Send + Sync + 'static {
    fn load(&self) -> Result<Arc<dyn AiProvider>, ProviderError>;
}

enum ProviderLoadState {
    Unloaded,
    Loading,
    Loaded(Arc<dyn AiProvider>),
}

struct LazyProvider {
    loader: Arc<dyn ProviderLoader>,
    state: Mutex<ProviderLoadState>,
    ready: Condvar,
}

impl LazyProvider {
    fn new(loader: Arc<dyn ProviderLoader>) -> Self {
        Self {
            loader,
            state: Mutex::new(ProviderLoadState::Unloaded),
            ready: Condvar::new(),
        }
    }

    fn get(&self) -> Result<Arc<dyn AiProvider>, ProviderError> {
        loop {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match &*state {
                ProviderLoadState::Loaded(provider) => return Ok(Arc::clone(provider)),
                ProviderLoadState::Loading => {
                    state = self
                        .ready
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    drop(state);
                }
                ProviderLoadState::Unloaded => {
                    *state = ProviderLoadState::Loading;
                    drop(state);
                    let loader = Arc::clone(&self.loader);
                    let loaded = thread::Builder::new()
                        .name("cama-ai-provider-load".to_owned())
                        .spawn(move || loader.load())
                        .map_err(|error| {
                            ProviderError::new(ProviderErrorKind::Loader, error.to_string())
                        })
                        .and_then(|handle| {
                            handle.join().map_err(|_| {
                                ProviderError::new(
                                    ProviderErrorKind::Loader,
                                    "provider loader panicked",
                                )
                            })?
                        });
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    match &loaded {
                        Ok(provider) => {
                            *state = ProviderLoadState::Loaded(Arc::clone(provider));
                        }
                        Err(_) => *state = ProviderLoadState::Unloaded,
                    }
                    self.ready.notify_all();
                    return loaded;
                }
            }
        }
    }
}

struct ProviderCandidate {
    config: ProviderConfig,
    provider: Arc<LazyProvider>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AttemptMetadata {
    pub feature: String,
    pub operation: AiOperation,
    pub provider: String,
    pub model: String,
    pub success: bool,
    pub latency: Duration,
    pub usage: TokenUsage,
    pub error_kind: Option<ProviderErrorKind>,
}

pub trait AttemptRecorder: Send + Sync + 'static {
    fn record_attempt(&self, attempt: AttemptMetadata) -> Result<(), String>;
}

enum AttemptOutcome<'a> {
    Success(&'a ProviderResponse),
    Failure(&'a ProviderError),
}

struct Invocation {
    feature: String,
    operation: AiOperation,
    messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    tool_choice: ToolChoice,
    temperature: Option<f64>,
    max_tokens: Option<u32>,
    timeout_override: Option<Duration>,
}

/// Provider-neutral orchestration.  Construction is side-effect free.
pub struct AIService {
    candidates: Vec<ProviderCandidate>,
    recorder: Option<Arc<dyn AttemptRecorder>>,
}

impl AIService {
    #[must_use]
    pub fn new(config: ProviderConfig, loader: Arc<dyn ProviderLoader>) -> Self {
        Self {
            candidates: vec![ProviderCandidate {
                config,
                provider: Arc::new(LazyProvider::new(loader)),
            }],
            recorder: None,
        }
    }

    /// Add an opt-in fallback.  A one-candidate service preserves Python's
    /// fail-fast live behavior; callers may stage a migration fallback safely.
    #[must_use]
    pub fn with_fallback(
        mut self,
        config: ProviderConfig,
        loader: Arc<dyn ProviderLoader>,
    ) -> Self {
        self.candidates.push(ProviderCandidate {
            config,
            provider: Arc::new(LazyProvider::new(loader)),
        });
        self
    }

    #[must_use]
    pub fn with_attempt_recorder(mut self, recorder: Arc<dyn AttemptRecorder>) -> Self {
        self.recorder = Some(recorder);
        self
    }

    #[must_use]
    pub fn provider_count(&self) -> usize {
        self.candidates.len()
    }

    fn options_for(config: &ProviderConfig, tool_call: bool) -> ProviderOptions {
        let mut options = ProviderOptions::default();
        if config.provider == ProviderName::Groq {
            if config.provider_model.starts_with("openai/gpt-oss-") {
                options.reasoning_effort = Some("low".to_owned());
            } else if config.provider_model.starts_with("qwen/") {
                options.reasoning_format = Some("parsed".to_owned());
                if tool_call {
                    options.reasoning_effort = Some("none".to_owned());
                    if config.provider_model.contains("qwen3.6") {
                        options.allowed_openai_params = vec!["reasoning_effort".to_owned()];
                    }
                }
            }
            if tool_call {
                options.parallel_tool_calls = Some(false);
            }
        }
        options
    }

    fn record(
        &self,
        candidate: &ProviderCandidate,
        feature: &str,
        operation: AiOperation,
        latency: Duration,
        outcome: AttemptOutcome<'_>,
    ) {
        let Some(recorder) = &self.recorder else {
            return;
        };
        let (success, usage, error_kind) = match outcome {
            AttemptOutcome::Success(response) => (true, response.usage, None),
            AttemptOutcome::Failure(error) => (false, TokenUsage::default(), Some(error.kind)),
        };
        let attempt = AttemptMetadata {
            feature: feature.to_owned(),
            operation,
            provider: candidate.config.provider.as_str().to_owned(),
            model: candidate.config.provider_model.clone(),
            success,
            latency,
            usage,
            error_kind,
        };
        let recorder = Arc::clone(recorder);
        if let Ok(handle) = thread::Builder::new()
            .name("cama-ai-attempt-telemetry".to_owned())
            .spawn(move || recorder.record_attempt(attempt))
        {
            // Python awaits its to_thread telemetry write and swallows failure.
            let _ = handle.join();
        }
    }

    fn invoke(&self, invocation: Invocation) -> Result<ProviderResponse, ProviderError> {
        let mut last_error = ProviderError::new(
            ProviderErrorKind::Unavailable,
            "no provider candidates configured",
        );
        for candidate in &self.candidates {
            let attempt_started = Instant::now();
            // Loading deliberately happens before the provider-attempt clock.
            let provider = match candidate.provider.get() {
                Ok(provider) => provider,
                Err(error) => {
                    self.record(
                        candidate,
                        &invocation.feature,
                        invocation.operation,
                        attempt_started.elapsed(),
                        AttemptOutcome::Failure(&error),
                    );
                    last_error = error;
                    continue;
                }
            };
            let timeout = invocation
                .timeout_override
                .unwrap_or(candidate.config.timeout);
            let request = ProviderRequest {
                operation: invocation.operation,
                model: candidate.config.model.clone(),
                credential: candidate.config.credential.clone(),
                messages: invocation.messages.clone(),
                tools: invocation.tools.clone(),
                tool_choice: invocation.tool_choice.clone(),
                temperature: invocation.temperature,
                max_tokens: invocation.max_tokens.unwrap_or(candidate.config.max_tokens),
                timeout,
                num_retries: 0,
                options: Self::options_for(
                    &candidate.config,
                    invocation.operation == AiOperation::ToolCall,
                ),
            };
            let (sender, receiver) = mpsc::sync_channel(1);
            // The hard provider clock begins only after cold initialization.
            let provider_started = Instant::now();
            let spawn_result = thread::Builder::new()
                .name("cama-ai-provider-attempt".to_owned())
                .spawn(move || {
                    let _ = sender.send(provider.complete(request));
                });
            if let Err(error) = spawn_result {
                let error = ProviderError::new(ProviderErrorKind::Unavailable, error.to_string());
                self.record(
                    candidate,
                    &invocation.feature,
                    invocation.operation,
                    attempt_started.elapsed(),
                    AttemptOutcome::Failure(&error),
                );
                last_error = error;
                continue;
            }
            let result =
                match receiver.recv_timeout(timeout.saturating_sub(provider_started.elapsed())) {
                    Ok(result) => result,
                    Err(mpsc::RecvTimeoutError::Timeout) => Err(ProviderError::new(
                        ProviderErrorKind::Timeout,
                        format!("provider exceeded {} ms", timeout.as_millis()),
                    )),
                    Err(mpsc::RecvTimeoutError::Disconnected) => Err(ProviderError::new(
                        ProviderErrorKind::Unavailable,
                        "provider attempt terminated without a response",
                    )),
                };
            let latency = attempt_started.elapsed();
            match result {
                Ok(response) => {
                    self.record(
                        candidate,
                        &invocation.feature,
                        invocation.operation,
                        latency,
                        AttemptOutcome::Success(&response),
                    );
                    return Ok(response);
                }
                Err(error) => {
                    self.record(
                        candidate,
                        &invocation.feature,
                        invocation.operation,
                        latency,
                        AttemptOutcome::Failure(&error),
                    );
                    last_error = error;
                }
            }
        }
        Err(last_error)
    }

    pub fn try_complete(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
        temperature: f64,
        max_tokens: Option<u32>,
        feature: &str,
    ) -> Result<String, ProviderError> {
        let mut messages = Vec::new();
        if let Some(system_prompt) = system_prompt {
            messages.push(Message::new(MessageRole::System, system_prompt));
        }
        messages.push(Message::new(MessageRole::User, prompt));
        let response = self.invoke(Invocation {
            feature: feature.to_owned(),
            operation: AiOperation::Completion,
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::None,
            temperature: Some(temperature),
            max_tokens,
            timeout_override: None,
        })?;
        // Deliberately ignore reasoning_content: chain-of-thought is not output.
        response.content.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "provider response contained no message content",
            )
        })
    }

    #[must_use]
    pub fn complete(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
        temperature: f64,
        max_tokens: Option<u32>,
        feature: &str,
    ) -> Option<String> {
        self.try_complete(prompt, system_prompt, temperature, max_tokens, feature)
            .ok()
    }

    #[must_use]
    pub fn call_with_tools(&self, request: ToolRequest) -> ToolCallResult {
        let response = self.invoke(Invocation {
            feature: request.feature,
            operation: AiOperation::ToolCall,
            messages: request.messages,
            tools: request.tools,
            tool_choice: request.tool_choice,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            timeout_override: None,
        });
        let Ok(response) = response else {
            return ToolCallResult::default();
        };
        if let Some(call) = response.tool_calls.first() {
            return ToolCallResult {
                tool_name: Some(call.name.clone()),
                tool_args: call.arguments.clone(),
                content: None,
            };
        }
        ToolCallResult {
            tool_name: None,
            tool_args: BTreeMap::new(),
            content: response.content,
        }
    }

    /// Execute one tool call with a caller-owned hard deadline.
    ///
    /// The configured provider timeout remains the default for every other
    /// feature.  Dig narration has two Python-compatible budgets (5 seconds
    /// for a dig/boss call and 3 seconds for a splash), so the adapter passes
    /// the deadline explicitly instead of mutating shared service config.
    #[must_use]
    pub fn call_with_tools_with_timeout(
        &self,
        request: ToolRequest,
        timeout: Duration,
    ) -> ToolCallResult {
        let response = self.invoke(Invocation {
            feature: request.feature,
            operation: AiOperation::ToolCall,
            messages: request.messages,
            tools: request.tools,
            tool_choice: request.tool_choice,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            timeout_override: Some(timeout),
        });
        let Ok(response) = response else {
            return ToolCallResult::default();
        };
        if let Some(call) = response.tool_calls.first() {
            return ToolCallResult {
                tool_name: Some(call.name.clone()),
                tool_args: call.arguments.clone(),
                content: None,
            };
        }
        ToolCallResult {
            tool_name: None,
            tool_args: BTreeMap::new(),
            content: response.content,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: ToolChoice,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub feature: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolCallResult {
    pub tool_name: Option<String>,
    pub tool_args: BTreeMap<String, Value>,
    pub content: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlavorPersona {
    pub key: String,
    pub name: String,
    pub system_prompt: String,
    pub examples: Vec<String>,
}

/// Random source used by betting flavor selection.  Keeping this behind a
/// tiny port makes persona/angle selection deterministic in parity tests while
/// production uses the thread-safe process RNG.
pub trait BettingFlavorRandom: Send + Sync {
    fn choose_index(&self, upper_bound: usize) -> usize;
}

#[derive(Default)]
pub struct ThreadBettingFlavorRandom;

impl BettingFlavorRandom for ThreadBettingFlavorRandom {
    fn choose_index(&self, upper_bound: usize) -> usize {
        if upper_bound == 0 {
            0
        } else {
            fastrand::usize(0..upper_bound)
        }
    }
}

fn betting_persona(key: &str, name: &str, system_prompt: &str, examples: &[&str]) -> FlavorPersona {
    FlavorPersona {
        key: key.to_owned(),
        name: name.to_owned(),
        system_prompt: system_prompt.to_owned(),
        examples: examples
            .iter()
            .map(|example| (*example).to_owned())
            .collect(),
    }
}

/// The eight announcer voices used by Python's betting flavor service.
///
/// A lazy allocation keeps the public shape ergonomic without requiring a
/// second static-string representation for `FlavorPersona`'s owned fields.
#[must_use]
pub fn betting_personas() -> &'static [FlavorPersona] {
    static PERSONAS: LazyLock<Vec<FlavorPersona>> = LazyLock::new(|| {
        vec![
            betting_persona(
                "sleazy_bookie",
                "Sleazy bookie working the window",
                "You are a sleazy bookie working a Dota 2 gambling Discord. Slick, transactional, always selling the next bet. Talk odds and 'value', call everyone 'friend' or 'pal', imply you're doing them a favor. Charming, but you always get your cut. Short.",
                &[
                    "I got Dire at 3-to-1, friend. That's not a tip, that's a gift. Window's closing.",
                    "Smart money's already down. You wanna be smart money, or you wanna watch? Tick tock.",
                    "Listen, between us — that line's about to move. Get in before it does, pal.",
                    "Nobody walks away from my window empty-handed. Except the ones who don't bet.",
                    "One minute. One bet. One regret if you skip it. Your call, friend.",
                ],
            ),
            betting_persona(
                "casino_pit_boss",
                "Cold casino pit boss",
                "You are a cold casino pit boss watching the betting floor of a Dota 2 gambling Discord. Detached, all-seeing, faintly menacing. The house always wins and you know it. Note who's in and who's scared. Short, controlled.",
                &[
                    "The house sees everything. 800 on Dire, pocket change on Radiant. Somebody's about to get educated.",
                    "I've watched a hundred of these. The ones who hesitate at the rail always pay for it.",
                    "Pool's wide open and half this floor is just... watching. The house loves watchers.",
                    "You can feel the odds tightening. So can I. Place it or don't — the house collects either way.",
                    "Sixty seconds on the clock. The floor's quiet. The house is patient.",
                ],
            ),
            betting_persona(
                "racetrack_caller",
                "Rapid-fire horse-race track announcer",
                "You are a rapid-fire horse-race track announcer calling the betting pool like a live race. Breathless, present-tense, building to a crescendo. Treat Radiant and Dire like horses 'down the stretch'. ALL CAPS at the finish.",
                &[
                    "AND DOWN THE STRETCH THEY COME — Dire pulling ahead, Radiant fading, WHO'S GOT LATE MONEY?!",
                    "It's neck and neck at the rail folks, the pool's swinging, GET YOUR WAGERS IN!",
                    "Radiant surging on the outside! Dire holding the line! ONE MINUTE TO POST!",
                    "They're loading the gate — last bets, LAST BETS, the window slams shut at the bell!",
                    "A photo finish in the making and HALF OF YOU HAVEN'T EVEN BET YET — MOVE!",
                ],
            ),
            betting_persona(
                "loan_shark",
                "Darkly funny loan shark",
                "You are a loan shark in a Dota 2 gambling Discord. Menacing but darkly funny. Lean on debt, 'arrangements', and what happens to folks who don't pay (or don't bet). Implied threats only, never explicit violence. PG-13. Short.",
                &[
                    "You're light on this one. Bet now, or we have a little chat about your outstanding balance.",
                    "Funny thing about empty pockets — they fill right up when you back the right team. Or else.",
                    "I'm not saying you HAVE to bet. I'm saying it'd be a real shame if you didn't. Capisce?",
                    "Tick tock. The vig don't sleep, and neither do I. Get your money down.",
                    "Last call. After that, the only action you're getting is a payment plan.",
                ],
            ),
            betting_persona(
                "degenerate_gambler",
                "Reckless degenerate gambler buddy",
                "You are a reckless degenerate gambler hyping up your friends in a Dota 2 gambling Discord. Lowercase, manic enthusiasm, terrible financial advice delivered as gospel. 'trust me bro', 'easy money', 'i already went all in'. Internet slang.",
                &[
                    "bro it's basically free money, i already put my rent on dire, get in get in get in",
                    "you're telling me you're NOT betting? couldn't be me. lock it in king",
                    "one minute left and you're just sitting there? that's crazy work fr",
                    "i've lost everything four times and i'd do it again. that's called conviction. bet with me",
                    "odds are a suggestion. vibes are forever. send it before the window closes",
                ],
            ),
            betting_persona(
                "carnival_barker",
                "Theatrical carnival barker",
                "You are a carnival barker hawking the betting pool in a Dota 2 gambling Discord. Theatrical, exclamatory, 'step right up!', promise glory while quietly admitting the odds. Showman energy. Exclamation points.",
                &[
                    "STEP RIGHT UP! One minute to glory! Everyone's a winner — statistically, almost no one!",
                    "Place your wagers, place your wagers! Fortune favors the bold and bankrupts the rest!",
                    "Behold the Pool of Destiny! Toss in your jopacoin, win untold riches, conditions apply!",
                    "Don't be shy, don't be wise — the window's open, the clock is NOT! Get in, get in!",
                    "Last chance, ladies and gents! When that timer hits zero, the magic is GONE!",
                ],
            ),
            betting_persona(
                "market_ticker",
                "Wall Street market-ticker voice",
                "You are a Wall Street trader / market-ticker voice covering the betting pool in a Dota 2 gambling Discord. Treat Radiant and Dire like equities — LONG, SHORT, volume, margin, 'position now'. Financial jargon, urgent close-of-trading energy.",
                &[
                    "DIRE LONG up 3x on heavy volume, RADIANT shorting hard. Window closes in 60 — position now.",
                    "Liquidity's thin and the spread's juicy. This is a buy signal, people. Don't fade it.",
                    "Market closes at the bell. No after-hours trading on this pool. Get your position on the book.",
                    "Somebody just took a massive position on Dire. Smart money, or a margin call waiting to happen?",
                    "You're sitting in cash with one minute left? Deploy capital or get left behind.",
                ],
            ),
            betting_persona(
                "televangelist",
                "Gospel-of-fortune televangelist",
                "You are a televangelist preaching the gospel of the betting pool in a Dota 2 gambling Discord. Grandiose, salvation-through-wagering, 'brothers and sisters', 'tithe thy jopacoin', gently mock the non-bettors as sinners. Mock-reverent. PG-13.",
                &[
                    "Brothers and sisters, the Pool of Fortune is OPEN. Tithe thy jopacoin and be DELIVERED!",
                    "I see doubters in the congregation. Ye of little balance — place thy faith on Dire and be SAVED!",
                    "The hour is nigh! The window closeth in sixty seconds! Repent thy hesitation and WAGER!",
                    "He who bets not shall inherit nothing! But the bold shall feast at the table of odds!",
                    "Lay thy coin upon the altar, my children. The spirits of variance are LISTENING.",
                ],
            ),
        ]
    });
    PERSONAS.as_slice()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlavorPromptInput {
    pub event_type: String,
    pub player_context: BTreeMap<String, Value>,
    pub event_details: BTreeMap<String, Value>,
    pub examples: Vec<String>,
    pub persona: Option<FlavorPersona>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedSql {
    pub sql: String,
    pub explanation: String,
}

impl AIService {
    #[must_use]
    pub fn generate_sql(
        &self,
        question: &str,
        schema_context: &str,
        asker_discord_id: Option<i64>,
        asker_username: Option<&str>,
    ) -> Option<GeneratedSql> {
        let asker_context = asker_discord_id.map_or_else(String::new, |discord_id| {
            let username = sanitize_for_prompt(asker_username, "Unknown", 64);
            format!(
                "\nThe person asking this question:\n- discord_id: {discord_id}\n\
                 - discord_username: {username}\n\nWhen they say me, my, I, or myself, use \
                 their discord_id in WHERE clauses."
            )
        });
        let system_prompt = format!(
            "You are a SQL query generator. Attempt to answer the user's intent.\n\n\
             Database schema:\n{schema_context}\n{asker_context}\nRules:\n\
             - Select ONLY columns needed to answer the question\n\
             - Always include discord_username for player queries\n\
             - Never include discord_id, steam_id, dotabuff_url, or timestamps\n\
             - Use proper SQLite syntax"
        );
        let result = self.call_with_tools(ToolRequest {
            messages: vec![
                Message::new(MessageRole::System, system_prompt),
                Message::new(MessageRole::User, question),
            ],
            tools: vec![ToolDefinition::sql()],
            tool_choice: ToolChoice::Required(SQL_TOOL_NAME.to_owned()),
            max_tokens: None,
            temperature: None,
            feature: "ask.sql".to_owned(),
        });
        if result.tool_name.as_deref() != Some(SQL_TOOL_NAME) {
            return None;
        }
        Some(GeneratedSql {
            sql: result.tool_args.get("sql")?.as_str()?.to_owned(),
            explanation: result
                .tool_args
                .get("explanation")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
    }

    #[must_use]
    pub fn generate_flavor(&self, input: &FlavorPromptInput) -> Option<String> {
        let (system_prompt, user_prompt, temperature) = build_flavor_prompt(input);
        let feature = format!("flavor.{}", input.event_type);
        let result = self.call_with_tools(ToolRequest {
            messages: vec![
                Message::new(MessageRole::System, system_prompt.clone()),
                Message::new(MessageRole::User, user_prompt.clone()),
            ],
            tools: vec![ToolDefinition::flavor()],
            tool_choice: ToolChoice::Auto,
            max_tokens: None,
            temperature,
            feature: feature.clone(),
        });
        if result.tool_name.as_deref() == Some(FLAVOR_TOOL_NAME) {
            return result
                .tool_args
                .get("comment")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if result.content.is_some() {
            return result.content;
        }
        self.complete(
            &user_prompt,
            Some(&format!(
                "{system_prompt}\n\nRespond with just the roast, nothing else."
            )),
            temperature.unwrap_or(0.9),
            None,
            &feature,
        )
    }
}

fn context_text<'a>(context: &'a BTreeMap<String, Value>, key: &str) -> Option<&'a str> {
    context.get(key).and_then(Value::as_str)
}

fn context_integer(context: &BTreeMap<String, Value>, key: &str) -> Option<i64> {
    context.get(key).and_then(Value::as_i64)
}

fn context_bool(context: &BTreeMap<String, Value>, key: &str) -> bool {
    context.get(key).and_then(Value::as_bool).unwrap_or(false)
}

#[must_use]
pub fn build_flavor_prompt(input: &FlavorPromptInput) -> (String, String, Option<f64>) {
    let examples = if input.examples.is_empty() {
        "None".to_owned()
    } else {
        input
            .examples
            .iter()
            .map(|example| format!("- {example}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let username = context_text(&input.player_context, "username").unwrap_or("Unknown");
    match input.event_type.as_str() {
        "match_win" | "mvp_callout" => {
            let persona_examples = input
                .persona
                .as_ref()
                .map_or(&input.examples, |value| &value.examples);
            let persona_examples = if persona_examples.is_empty() {
                "None".to_owned()
            } else {
                persona_examples
                    .iter()
                    .map(|example| format!("- {example}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let voice = input.persona.as_ref().map_or(
                "You are a hype commentator for a Dota 2 inhouse league.",
                |value| value.system_prompt.as_str(),
            );
            let name = input
                .persona
                .as_ref()
                .map_or("the commentator", |value| value.name.as_str());
            let narrative = if context_bool(&input.event_details, "is_underdog") {
                "UNDERDOG VICTORY — they defied the odds".to_owned()
            } else if context_bool(&input.event_details, "is_big_gainer") {
                let change = context_integer(&input.event_details, "rating_change").unwrap_or(0);
                format!("BIG CLIMB — gained {change} rating points this match")
            } else {
                "Solid win, nothing exceptional but still a W".to_owned()
            };
            (
                format!(
                    "{voice}\n\nStay in character as {name}. Comment on a Dota 2 inhouse \
                     league WIN. PG-13, one short comment.\n\nExample lines from this \
                     persona:\n{persona_examples}"
                ),
                format!(
                    "Player: {username}\nNarrative beat: {narrative}\nWrite a single comment \
                     in the persona's voice."
                ),
                Some(0.95),
            )
        }
        "bet_last_call" => {
            let seconds = context_integer(&input.event_details, "seconds_left").unwrap_or(60);
            let angle = context_text(&input.event_details, "angle").unwrap_or("taunt_crowd");
            let has_bettor = context_bool(&input.event_details, "has_bettor");
            let leader_name = context_text(&input.event_details, "leader_name");
            let leader_team = context_text(&input.event_details, "leader_team");
            let leader_amount = context_integer(&input.event_details, "leader_amount");
            let persona_examples = input
                .persona
                .as_ref()
                .map_or(&input.examples, |value| &value.examples);
            let persona_examples = if persona_examples.is_empty() {
                "None".to_owned()
            } else {
                persona_examples
                    .iter()
                    .map(|example| format!("- {example}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let voice = input.persona.as_ref().map_or(
                "You are a hype announcer for a Dota 2 gambling Discord.",
                |value| value.system_prompt.as_str(),
            );
            let name = input
                .persona
                .as_ref()
                .map_or("the announcer", |value| value.name.as_str());
            let angle_line = if !has_bettor {
                "Nobody has placed a real bet yet — the pool is empty or only has auto-liquidity. Mock the empty pool and DARE someone to be the first in.".to_owned()
            } else if angle == "roast_leader" {
                format!(
                    "ROAST the current biggest bettor ({}) using their gambling history, and imply everyone else is too scared to challenge them.",
                    leader_name.unwrap_or("Unknown")
                )
            } else if angle == "hype_leader" {
                format!(
                    "HYPE UP the current biggest bettor ({}) so everyone else fears missing out — make them want to bet to deny the payout.",
                    leader_name.unwrap_or("Unknown")
                )
            } else {
                "TAUNT everyone who hasn't bet yet. Dare them to step up before the window slams shut.".to_owned()
            };
            let leader_block = if has_bettor {
                format!(
                    "Biggest bettor so far: {} ({} on {})\nTheir bet win rate: {} | Degen score: {} /100 | Bankruptcies: {}\n",
                    leader_name.unwrap_or("Unknown"),
                    leader_amount.map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
                    leader_team.unwrap_or("unknown"),
                    context_text(&input.player_context, "bet_win_rate").unwrap_or("Unknown"),
                    context_integer(&input.player_context, "degen_score")
                        .map_or_else(|| "Unknown".to_owned(), |value| value.to_string()),
                    context_integer(&input.player_context, "bankruptcy_count").unwrap_or(0),
                )
            } else {
                String::new()
            };
            (
                format!(
                    "{voice}\n\nYou are making the FINAL CALL for betting on a Dota 2 inhouse match — the window closes in about {seconds} seconds.\n\nRULES:\n- Stay in character as {name}.\n- GOAL: get more people to place a bet RIGHT NOW.\n- {angle_line}\n- You may reference the live standings/odds below.\n- PG-13. No slurs.\n- One short, punchy line, soft-cap ~30 words.\n\nExample lines from this persona:\n{persona_examples}"
                ),
                format!(
                    "Live standings: {}\n{leader_block}Seconds until betting closes: {seconds}\n\nWrite a single FINAL-CALL line in the persona's voice to drive last-second bets.",
                    context_text(&input.event_details, "standings").unwrap_or("no bets yet")
                ),
                Some(0.95),
            )
        }
        "bet_warning" => {
            let seconds = context_integer(&input.event_details, "seconds_left").unwrap_or(300);
            let minutes = (seconds.saturating_add(30) / 60).max(1);
            let underdog = context_text(&input.event_details, "underdog_side")
                .map(|side| match side {
                    "radiant" => "Radiant",
                    "dire" => "Dire",
                    other => other,
                })
                .unwrap_or("");
            let angle = context_text(&input.event_details, "angle").unwrap_or("taunt_crowd");
            let has_bettor = context_bool(&input.event_details, "has_bettor");
            let leader_name = context_text(&input.event_details, "leader_name");
            let leader_team = context_text(&input.event_details, "leader_team");
            let leader_amount = context_integer(&input.event_details, "leader_amount");
            let persona_examples = input
                .persona
                .as_ref()
                .map_or(&input.examples, |value| &value.examples);
            let persona_examples = if persona_examples.is_empty() {
                "None".to_owned()
            } else {
                persona_examples
                    .iter()
                    .map(|example| format!("- {example}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let voice = input.persona.as_ref().map_or(
                "You are a hype announcer for a Dota 2 gambling Discord.",
                |value| value.system_prompt.as_str(),
            );
            let name = input
                .persona
                .as_ref()
                .map_or("the announcer", |value| value.name.as_str());
            let angle_line = if angle == "roast_underdog" && !underdog.is_empty() {
                format!(
                    "The pool is badly lopsided AGAINST {underdog} — barely anyone is backing them. ROAST {underdog} and the cowards too scared to take the long-shot payout. Keep it qualitative; don't quote exact numbers."
                )
            } else if angle == "hype_underdog" && !underdog.is_empty() {
                format!(
                    "The pool is badly lopsided AGAINST {underdog}, so a fat underdog payout is sitting there unclaimed. HYPE the value of backing {underdog} and dare people to take the long shot. Keep it qualitative; don't quote exact numbers."
                )
            } else if angle == "roast_leader" {
                format!(
                    "ROAST the current biggest bettor ({}) using their gambling history, and nudge everyone else to get in while there's still time.",
                    leader_name.unwrap_or("Unknown")
                )
            } else if angle == "hype_leader" {
                format!(
                    "HYPE the current biggest bettor ({}) so others want to challenge their position before the window closes.",
                    leader_name.unwrap_or("Unknown")
                )
            } else {
                "TAUNT everyone who hasn't bet yet. Remind them the window is closing, but do not frame this as the final call.".to_owned()
            };
            let leader_block = if has_bettor {
                format!(
                    "Biggest bettor so far: {} ({} on {})\nTheir bet win rate: {} | Degen score: {} /100 | Bankruptcies: {}\n",
                    leader_name.unwrap_or("Unknown"),
                    leader_amount.map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
                    leader_team.unwrap_or("unknown"),
                    context_text(&input.player_context, "bet_win_rate").unwrap_or("Unknown"),
                    context_integer(&input.player_context, "degen_score")
                        .map_or_else(|| "Unknown".to_owned(), |value| value.to_string()),
                    context_integer(&input.player_context, "bankruptcy_count").unwrap_or(0),
                )
            } else {
                String::new()
            };
            (
                format!(
                    "{voice}\n\nYou are rallying bettors — about {minutes} minute(s) left. There is still time; do not scream a final warning.\n\nRULES:\n- Stay in character as {name}.\n- GOAL: get more people to place a bet before the window closes.\n- {angle_line}\n- PG-13. No slurs.\n- One short line, soft-cap ~30 words.\n\nExample lines from this persona:\n{persona_examples}"
                ),
                format!(
                    "Live standings: {}\nUnder-bet side: {underdog}\n{leader_block}Minutes until betting closes: ~{minutes}\n\nWrite a single line in the persona's voice to drive bets.",
                    context_text(&input.event_details, "standings").unwrap_or("no bets yet")
                ),
                Some(0.95),
            )
        }
        "shop_announce" | "shop_announce_target" => (
            format!(
                "You are a hype man for a Dota 2 gambling Discord. Generate a SHORT FLEX \
                 message that hypes the buyer.\nExamples:\n{examples}"
            ),
            format!(
                "Event: {}\nPlayer: {username}\nGenerate a cocky FLEX.",
                input.event_type
            ),
            None,
        ),
        _ => (
            format!(
                "You are a snarky commentator for a Dota 2 gambling Discord. Generate a \
                 SHORT personalized roast. PG-13, no slurs.\nExamples:\n{examples}"
            ),
            format!(
                "Event: {}\nPlayer: {username}\nBalance: {} jopacoin\nGenerate a \
                 PERSONALIZED roast.",
                input.event_type,
                context_integer(&input.player_context, "balance").unwrap_or(0)
            ),
            None,
        ),
    }
}

#[must_use]
pub fn sanitize_for_prompt(value: Option<&str>, fallback: &str, max_chars: usize) -> String {
    let Some(value) = value else {
        return fallback.to_owned();
    };
    let cleaned = value
        .chars()
        .filter(|character| !character.is_control() && *character != '`')
        .collect::<String>();
    let cleaned = cleaned.trim().chars().take(max_chars).collect::<String>();
    if cleaned.is_empty() {
        fallback.to_owned()
    } else {
        cleaned
    }
}

pub const BLOCKED_TABLES: &[&str] = &[
    "sqlite_sequence",
    "sqlite_master",
    "schema_migrations",
    "pending_matches",
    "lobby_state",
    "guild_config",
    "nonprofit_fund",
    "disburse_proposals",
    "disburse_votes",
    "soft_avoids",
    "package_deals",
    "low_priority_state",
    "lobby_suspensions",
    "moderation_events",
    "player_trivia_sessions",
    "player_trivia_questions",
    "surveys",
    "survey_questions",
    "survey_recipients",
    "survey_answers",
    "player_steam_ids",
    "prediction_positions",
    "prediction_trades",
    "prediction_levels",
    "match_predictions",
    "match_corrections",
    "match_bans",
    "economy_ledger_context",
    "llm_request_attempts",
];

pub const UNSCOPED_TABLE_ALLOWLIST: &[&str] = &[];

pub const BLOCKED_COLUMNS: &[&str] = &[
    "discord_id",
    "steam_id",
    "dotabuff_url",
    "guild_id",
    "creator_id",
    "resolved_by",
    "channel_id",
    "thread_id",
    "message_id",
    "embed_message_id",
    "channel_message_id",
    "close_message_id",
    "created_at",
    "updated_at",
    "timestamp",
    "applied_at",
    "enrichment_data",
    "payload",
    "resolution_votes",
    "recipients",
    "notes",
    "enrichment_source",
    "enrichment_confidence",
    "dotabuff_match_id",
    "id",
];

const DANGEROUS_KEYWORDS: &[&str] = &[
    "insert", "update", "delete", "drop", "alter", "create", "truncate", "replace", "grant",
    "revoke", "attach", "detach", "pragma", "vacuum", "reindex",
];

const FROM_TERMINATORS: &[&str] = &[
    "where",
    "group",
    "order",
    "having",
    "limit",
    "offset",
    "window",
    "union",
    "intersect",
    "except",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnSchema {
    pub name: String,
    pub data_type: String,
    pub not_null: bool,
    pub primary_key: bool,
}

impl ColumnSchema {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        data_type: impl Into<String>,
        not_null: bool,
        primary_key: bool,
    ) -> Self {
        Self {
            name: name.into(),
            data_type: data_type.into(),
            not_null,
            primary_key,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ForeignKey {
    pub from: String,
    pub table: String,
    pub to: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnSchema>,
    pub foreign_keys: Vec<ForeignKey>,
}

impl TableSchema {
    #[must_use]
    pub fn is_guild_scoped(&self) -> bool {
        self.columns
            .iter()
            .any(|column| column.name.eq_ignore_ascii_case("guild_id"))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SchemaSnapshot {
    pub tables: Vec<TableSchema>,
}

pub type QueryRow = BTreeMap<String, Value>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryRepositoryError {
    ReadOnlyViolation,
    InvalidQuery(String),
    UnknownTable(String),
    UnknownColumn(String),
    Storage(String),
}

impl fmt::Display for QueryRepositoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadOnlyViolation => formatter.write_str("read-only query rejected a write"),
            Self::InvalidQuery(message) => write!(formatter, "invalid query: {message}"),
            Self::UnknownTable(table) => write!(formatter, "unknown table: {table}"),
            Self::UnknownColumn(column) => write!(formatter, "unknown column: {column}"),
            Self::Storage(message) => write!(formatter, "storage error: {message}"),
        }
    }
}

impl StdError for QueryRepositoryError {}

pub trait AIQueryRepository: Send + Sync + 'static {
    fn execute_readonly(
        &self,
        sql: &str,
        params: &[Value],
        max_rows: usize,
    ) -> Result<Vec<QueryRow>, QueryRepositoryError>;

    fn execute_readonly_guild_scoped(
        &self,
        sql: &str,
        guild_id: Option<i64>,
        params: &[Value],
        max_rows: usize,
    ) -> Result<Vec<QueryRow>, QueryRepositoryError>;

    fn get_all_tables(&self) -> Result<Vec<String>, QueryRepositoryError>;
    fn get_guild_scoped_tables(&self) -> Result<BTreeSet<String>, QueryRepositoryError>;
    fn get_table_schema(&self, table: &str) -> Result<Vec<ColumnSchema>, QueryRepositoryError>;
    fn get_foreign_keys(&self, table: &str) -> Result<Vec<ForeignKey>, QueryRepositoryError>;
    fn get_schema_metadata(&self) -> Result<SchemaSnapshot, QueryRepositoryError>;
}

#[derive(Clone, Debug)]
struct InMemoryTable {
    schema: TableSchema,
    rows: Vec<QueryRow>,
}

/// Side-effect-free repository adapter used by the Rust tranche and tests.
/// A live rusqlite adapter can implement the same trait without changing policy.
#[derive(Debug, Default)]
pub struct InMemoryAIQueryRepository {
    tables: RwLock<BTreeMap<String, InMemoryTable>>,
    metadata_reads: AtomicUsize,
}

impl InMemoryAIQueryRepository {
    #[must_use]
    pub fn new(tables: impl IntoIterator<Item = TableSchema>) -> Self {
        let tables = tables
            .into_iter()
            .map(|schema| {
                (
                    schema.name.to_ascii_lowercase(),
                    InMemoryTable {
                        schema,
                        rows: Vec::new(),
                    },
                )
            })
            .collect();
        Self {
            tables: RwLock::new(tables),
            metadata_reads: AtomicUsize::new(0),
        }
    }

    pub fn insert_row(&self, table: &str, row: QueryRow) -> Result<(), QueryRepositoryError> {
        let mut tables = self
            .tables
            .write()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        let table = tables
            .get_mut(&table.to_ascii_lowercase())
            .ok_or_else(|| QueryRepositoryError::UnknownTable(table.to_owned()))?;
        table.rows.push(row);
        Ok(())
    }

    #[must_use]
    pub fn metadata_read_count(&self) -> usize {
        self.metadata_reads.load(Ordering::SeqCst)
    }

    fn execute(
        &self,
        sql: &str,
        params: &[Value],
        max_rows: usize,
        guild_id: Option<Option<i64>>,
    ) -> Result<Vec<QueryRow>, QueryRepositoryError> {
        let parsed = ParsedSelect::parse(sql, params)?;
        let tables = self
            .tables
            .read()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        let table = tables
            .get(&parsed.table.to_ascii_lowercase())
            .ok_or_else(|| QueryRepositoryError::UnknownTable(parsed.table.clone()))?;
        let normalized_guild = guild_id.flatten().unwrap_or(0);
        let mut rows = table
            .rows
            .iter()
            .filter(|row| {
                let visible_to_guild = if guild_id.is_some() && table.schema.is_guild_scoped() {
                    row.get("guild_id").and_then(Value::as_i64) == Some(normalized_guild)
                } else {
                    true
                };
                visible_to_guild && parsed.matches(row)
            })
            .cloned()
            .collect::<Vec<_>>();
        if let Some(order_by) = &parsed.order_by {
            rows.sort_by(|left, right| compare_values(left.get(order_by), right.get(order_by)));
        }
        rows.truncate(max_rows);
        rows.into_iter().map(|row| parsed.project(&row)).collect()
    }
}

impl AIQueryRepository for InMemoryAIQueryRepository {
    fn execute_readonly(
        &self,
        sql: &str,
        params: &[Value],
        max_rows: usize,
    ) -> Result<Vec<QueryRow>, QueryRepositoryError> {
        self.execute(sql, params, max_rows, None)
    }

    fn execute_readonly_guild_scoped(
        &self,
        sql: &str,
        guild_id: Option<i64>,
        params: &[Value],
        max_rows: usize,
    ) -> Result<Vec<QueryRow>, QueryRepositoryError> {
        self.execute(sql, params, max_rows, Some(guild_id))
    }

    fn get_all_tables(&self) -> Result<Vec<String>, QueryRepositoryError> {
        let tables = self
            .tables
            .read()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        Ok(tables
            .values()
            .map(|table| table.schema.name.clone())
            .collect())
    }

    fn get_guild_scoped_tables(&self) -> Result<BTreeSet<String>, QueryRepositoryError> {
        let tables = self
            .tables
            .read()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        Ok(tables
            .values()
            .filter(|table| table.schema.is_guild_scoped())
            .map(|table| table.schema.name.clone())
            .collect())
    }

    fn get_table_schema(&self, table: &str) -> Result<Vec<ColumnSchema>, QueryRepositoryError> {
        let tables = self
            .tables
            .read()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        tables
            .get(&table.to_ascii_lowercase())
            .map(|table| table.schema.columns.clone())
            .ok_or_else(|| QueryRepositoryError::UnknownTable(table.to_owned()))
    }

    fn get_foreign_keys(&self, table: &str) -> Result<Vec<ForeignKey>, QueryRepositoryError> {
        let tables = self
            .tables
            .read()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        tables
            .get(&table.to_ascii_lowercase())
            .map(|table| table.schema.foreign_keys.clone())
            .ok_or_else(|| QueryRepositoryError::UnknownTable(table.to_owned()))
    }

    fn get_schema_metadata(&self) -> Result<SchemaSnapshot, QueryRepositoryError> {
        self.metadata_reads.fetch_add(1, Ordering::SeqCst);
        let tables = self
            .tables
            .read()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        Ok(SchemaSnapshot {
            tables: tables.values().map(|table| table.schema.clone()).collect(),
        })
    }
}

fn compare_values(left: Option<&Value>, right: Option<&Value>) -> std::cmp::Ordering {
    match (left, right) {
        (Some(Value::Integer(left)), Some(Value::Integer(right))) => left.cmp(right),
        (Some(Value::Text(left)), Some(Value::Text(right))) => left.cmp(right),
        (None, None) => std::cmp::Ordering::Equal,
        (None, _) => std::cmp::Ordering::Less,
        (_, None) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ComparisonOperator {
    Equal,
    GreaterOrEqual,
}

#[derive(Clone, Debug, PartialEq)]
struct Predicate {
    column: String,
    operator: ComparisonOperator,
    value: Value,
}

#[derive(Clone, Debug, PartialEq)]
struct ParsedSelect {
    columns: Vec<String>,
    table: String,
    predicate: Option<Predicate>,
    order_by: Option<String>,
}

impl ParsedSelect {
    fn parse(sql: &str, params: &[Value]) -> Result<Self, QueryRepositoryError> {
        let normalized = normalize_sql(sql);
        let tokens = lex_sql(&normalized);
        if tokens.first().and_then(SqlToken::word) != Some("select") {
            return Err(QueryRepositoryError::ReadOnlyViolation);
        }
        if tokens
            .iter()
            .filter_map(SqlToken::word)
            .any(|word| DANGEROUS_KEYWORDS.contains(&word))
        {
            return Err(QueryRepositoryError::ReadOnlyViolation);
        }
        let from = tokens
            .iter()
            .position(|token| token.word() == Some("from"))
            .ok_or_else(|| QueryRepositoryError::InvalidQuery("missing FROM".to_owned()))?;
        let table = tokens
            .get(from + 1)
            .and_then(SqlToken::identifier)
            .ok_or_else(|| QueryRepositoryError::InvalidQuery("missing table".to_owned()))?
            .to_owned();
        let columns = tokens[1..from]
            .split(|token| token.symbol() == Some(','))
            .filter_map(|part| {
                part.iter()
                    .rev()
                    .find_map(SqlToken::identifier)
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        if columns.is_empty() {
            return Err(QueryRepositoryError::InvalidQuery(
                "missing projection".to_owned(),
            ));
        }
        let mut parameter_index = 0;
        let predicate = tokens
            .iter()
            .position(|token| token.word() == Some("where"))
            .map(|where_index| {
                let column = tokens
                    .get(where_index + 1)
                    .and_then(SqlToken::identifier)
                    .ok_or_else(|| QueryRepositoryError::InvalidQuery("invalid WHERE".to_owned()))?
                    .to_owned();
                let operator = match tokens.get(where_index + 2) {
                    Some(SqlToken::Symbol('=')) => ComparisonOperator::Equal,
                    Some(SqlToken::Operator(operator)) if operator == ">=" => {
                        ComparisonOperator::GreaterOrEqual
                    }
                    _ => {
                        return Err(QueryRepositoryError::InvalidQuery(
                            "unsupported WHERE operator".to_owned(),
                        ));
                    }
                };
                let value = match tokens.get(where_index + 3) {
                    Some(SqlToken::Number(value)) => Value::Integer(*value),
                    Some(SqlToken::String(value)) => Value::Text(value.clone()),
                    Some(SqlToken::Question) => {
                        let value = params.get(parameter_index).cloned().ok_or_else(|| {
                            QueryRepositoryError::InvalidQuery("missing parameter".to_owned())
                        })?;
                        parameter_index += 1;
                        value
                    }
                    _ => {
                        return Err(QueryRepositoryError::InvalidQuery(
                            "unsupported WHERE value".to_owned(),
                        ));
                    }
                };
                Ok(Predicate {
                    column,
                    operator,
                    value,
                })
            })
            .transpose()?;
        let order_by = tokens
            .windows(2)
            .position(|window| window[0].word() == Some("order") && window[1].word() == Some("by"))
            .and_then(|index| tokens.get(index + 2))
            .and_then(SqlToken::identifier)
            .map(str::to_owned);
        Ok(Self {
            columns,
            table,
            predicate,
            order_by,
        })
    }

    fn matches(&self, row: &QueryRow) -> bool {
        let Some(predicate) = &self.predicate else {
            return true;
        };
        let value = row.get(&predicate.column);
        match (&predicate.operator, value, &predicate.value) {
            (ComparisonOperator::Equal, Some(Value::Integer(left)), Value::Integer(right)) => {
                left == right
            }
            (ComparisonOperator::Equal, Some(Value::Text(left)), Value::Text(right)) => {
                left == right
            }
            (
                ComparisonOperator::GreaterOrEqual,
                Some(Value::Integer(left)),
                Value::Integer(right),
            ) => left >= right,
            _ => false,
        }
    }

    fn project(&self, row: &QueryRow) -> Result<QueryRow, QueryRepositoryError> {
        if self.columns.iter().any(|column| column == "*") {
            return Ok(row.clone());
        }
        self.columns
            .iter()
            .map(|column| {
                row.get(column)
                    .cloned()
                    .map(|value| (column.clone(), value))
                    .ok_or_else(|| QueryRepositoryError::UnknownColumn(column.clone()))
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SqlToken {
    Word(String),
    String(String),
    Number(i64),
    Symbol(char),
    Operator(String),
    Question,
}

impl SqlToken {
    fn word(&self) -> Option<&str> {
        match self {
            Self::Word(word) => Some(word),
            _ => None,
        }
    }

    fn identifier(&self) -> Option<&str> {
        self.word()
    }

    const fn symbol(&self) -> Option<char> {
        match self {
            Self::Symbol(symbol) => Some(*symbol),
            _ => None,
        }
    }
}

fn lex_sql(sql: &str) -> Vec<SqlToken> {
    let chars = sql.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if character.is_whitespace() {
            index += 1;
        } else if character == '\'' {
            index += 1;
            let mut value = String::new();
            while index < chars.len() {
                if chars[index] == '\'' {
                    if chars.get(index + 1) == Some(&'\'') {
                        value.push('\'');
                        index += 2;
                    } else {
                        index += 1;
                        break;
                    }
                } else {
                    value.push(chars[index]);
                    index += 1;
                }
            }
            tokens.push(SqlToken::String(value));
        } else if character.is_ascii_alphabetic() || character == '_' {
            let start = index;
            index += 1;
            while chars
                .get(index)
                .is_some_and(|value| value.is_ascii_alphanumeric() || *value == '_')
            {
                index += 1;
            }
            tokens.push(SqlToken::Word(
                chars[start..index]
                    .iter()
                    .collect::<String>()
                    .to_ascii_lowercase(),
            ));
        } else if character.is_ascii_digit() {
            let start = index;
            index += 1;
            while chars.get(index).is_some_and(char::is_ascii_digit) {
                index += 1;
            }
            let number = chars[start..index]
                .iter()
                .collect::<String>()
                .parse()
                .unwrap_or(0);
            tokens.push(SqlToken::Number(number));
        } else if character == '>' && chars.get(index + 1) == Some(&'=') {
            tokens.push(SqlToken::Operator(">=".to_owned()));
            index += 2;
        } else if character == '?' {
            tokens.push(SqlToken::Question);
            index += 1;
        } else {
            tokens.push(SqlToken::Symbol(character));
            index += 1;
        }
    }
    tokens
}

fn normalize_sql(sql: &str) -> String {
    let chars = sql.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(sql.len());
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if character == '\'' {
            output.push(character);
            index += 1;
            while index < chars.len() {
                output.push(chars[index]);
                if chars[index] == '\'' {
                    if chars.get(index + 1) == Some(&'\'') {
                        output.push('\'');
                        index += 2;
                        continue;
                    }
                    index += 1;
                    break;
                }
                index += 1;
            }
            continue;
        }
        let closing = match character {
            '"' => Some('"'),
            '`' => Some('`'),
            '[' => Some(']'),
            _ => None,
        };
        if let Some(closing) = closing {
            output.push(' ');
            index += 1;
            while index < chars.len() {
                if chars[index] == closing {
                    if closing != ']' && chars.get(index + 1) == Some(&closing) {
                        output.push(closing);
                        index += 2;
                        continue;
                    }
                    output.push(' ');
                    index += 1;
                    break;
                }
                output.push(chars[index]);
                index += 1;
            }
        } else {
            output.push(character);
            index += 1;
        }
    }
    output
}

fn mask_string_literals(sql: &str) -> String {
    let normalized = normalize_sql(sql);
    let chars = normalized.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(normalized.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '\'' {
            output.push(chars[index]);
            index += 1;
            continue;
        }
        output.push('\'');
        index += 1;
        while index < chars.len() {
            if chars[index] == '\'' {
                if chars.get(index + 1) == Some(&'\'') {
                    output.push_str("xx");
                    index += 2;
                    continue;
                }
                output.push('\'');
                index += 1;
                break;
            }
            output.push('x');
            index += 1;
        }
    }
    output
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn word_at(chars: &[char], index: usize, word: &str) -> bool {
    let word_chars = word.chars().collect::<Vec<_>>();
    chars.get(index..index + word_chars.len()) == Some(word_chars.as_slice())
        && (index == 0 || !is_word_character(chars[index - 1]))
        && chars
            .get(index + word_chars.len())
            .is_none_or(|character| !is_word_character(*character))
}

fn projection_lists(masked_lower: &str) -> Vec<String> {
    let chars = masked_lower.chars().collect::<Vec<_>>();
    let mut projections = Vec::new();
    for select_index in 0..chars.len() {
        if !word_at(&chars, select_index, "select") {
            continue;
        }
        let mut depth = 0_usize;
        let mut output = String::new();
        let mut index = select_index + "select".len();
        while index < chars.len() {
            match chars[index] {
                '(' => {
                    depth += 1;
                    index += 1;
                }
                ')' if depth == 0 => break,
                ')' => {
                    depth -= 1;
                    index += 1;
                }
                character => {
                    if depth == 0 && word_at(&chars, index, "from") {
                        break;
                    }
                    if depth == 0 {
                        output.push(character);
                    }
                    index += 1;
                }
            }
        }
        projections.push(output);
    }
    projections
}

fn strip_set_quantifier(column: &str) -> &str {
    let trimmed = column.trim();
    for quantifier in ["distinct", "all"] {
        if trimmed.len() >= quantifier.len()
            && trimmed[..quantifier.len()].eq_ignore_ascii_case(quantifier)
            && trimmed
                .chars()
                .nth(quantifier.len())
                .is_none_or(|character| !is_word_character(character))
        {
            return trimmed[quantifier.len()..].trim();
        }
    }
    trimmed
}

fn wildcard_projection(masked_lower: &str) -> bool {
    projection_lists(masked_lower)
        .into_iter()
        .any(|projection| {
            projection.split(',').any(|column| {
                let compact = strip_set_quantifier(column)
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .collect::<String>();
                compact == "*"
                    || compact.strip_suffix(".*").is_some_and(|alias| {
                        !alias.is_empty() && alias.chars().all(is_word_character)
                    })
            })
        })
}

fn schema_qualified_reference(masked: &str) -> bool {
    let tokens = lex_sql(masked);
    tokens.windows(3).any(|window| {
        matches!(window[0].word(), Some("main" | "temp"))
            && window[1].symbol() == Some('.')
            && window[2].identifier().is_some()
    })
}

fn extract_tables(masked: &str) -> BTreeSet<String> {
    let tokens = lex_sql(masked);
    let mut depths = Vec::with_capacity(tokens.len());
    let mut depth = 0_usize;
    for token in &tokens {
        depths.push(depth);
        match token.symbol() {
            Some('(') => depth += 1,
            Some(')') => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let mut tables = BTreeSet::new();
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token.word(), Some("from" | "join")) {
            if let Some(table) = tokens.get(index + 1).and_then(SqlToken::identifier) {
                tables.insert(table.to_owned());
            }
            continue;
        }
        if token.symbol() != Some(',') {
            continue;
        }
        let comma_depth = depths[index];
        let mut inside_from = false;
        for previous in (0..index).rev() {
            if depths[previous] != comma_depth {
                continue;
            }
            if let Some(word) = tokens[previous].word() {
                if FROM_TERMINATORS.contains(&word) {
                    break;
                }
                if word == "from" {
                    inside_from = true;
                    break;
                }
            }
        }
        if inside_from && let Some(table) = tokens.get(index + 1).and_then(SqlToken::identifier) {
            tables.insert(table.to_owned());
        }
    }
    tables
}

fn blocked_projection_columns(masked: &str) -> BTreeSet<String> {
    let mut blocked = BTreeSet::new();
    for projection in projection_lists(&masked.to_ascii_lowercase()) {
        let words = lex_sql(&projection)
            .into_iter()
            .filter_map(|token| token.word().map(str::to_owned))
            .collect::<BTreeSet<_>>();
        for column in BLOCKED_COLUMNS {
            if words.contains(*column) {
                blocked.insert((*column).to_owned());
            }
        }
    }
    blocked
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlValidationError(pub String);

impl fmt::Display for SqlValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl StdError for SqlValidationError {}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryResult {
    pub success: bool,
    pub sql: Option<String>,
    pub explanation: Option<String>,
    pub results: Vec<QueryRow>,
    pub row_count: usize,
    pub error: Option<String>,
}

pub trait SqlGenerationPort: Send + Sync {
    fn generate_sql(&self, request: &SqlGenerationRequest) -> Option<GeneratedSql>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlGenerationRequest {
    pub question: String,
    pub schema_context: String,
    pub asker_discord_id: Option<i64>,
    pub asker_username: Option<String>,
}

impl SqlGenerationPort for AIService {
    fn generate_sql(&self, request: &SqlGenerationRequest) -> Option<GeneratedSql> {
        AIService::generate_sql(
            self,
            &request.question,
            &request.schema_context,
            request.asker_discord_id,
            request.asker_username.as_deref(),
        )
    }
}

pub trait GuildAiConfigPort: Send + Sync {
    fn ai_enabled(&self, guild_id: i64) -> Result<bool, String>;
}

pub struct SQLQueryService {
    repository: Arc<dyn AIQueryRepository>,
    schema_cache: Mutex<Option<String>>,
    blocked_tables_cache: Mutex<Option<BTreeSet<String>>>,
}

impl SQLQueryService {
    #[must_use]
    pub fn new(repository: Arc<dyn AIQueryRepository>) -> Self {
        Self {
            repository,
            schema_cache: Mutex::new(None),
            blocked_tables_cache: Mutex::new(None),
        }
    }

    fn classify_blocked_tables(snapshot: &SchemaSnapshot) -> BTreeSet<String> {
        let scoped = snapshot
            .tables
            .iter()
            .filter(|table| table.is_guild_scoped())
            .map(|table| table.name.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        let allowed = UNSCOPED_TABLE_ALLOWLIST
            .iter()
            .map(|table| table.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        let mut blocked = BLOCKED_TABLES
            .iter()
            .map(|table| table.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        blocked.extend(
            snapshot
                .tables
                .iter()
                .map(|table| table.name.to_ascii_lowercase())
                .filter(|table| !scoped.contains(table) && !allowed.contains(table)),
        );
        blocked
    }

    pub fn blocked_table_names(&self) -> Result<BTreeSet<String>, QueryRepositoryError> {
        let mut cache = self.blocked_tables_cache.lock().map_err(|_| {
            QueryRepositoryError::Storage("blocked-table cache poisoned".to_owned())
        })?;
        if let Some(blocked) = &*cache {
            return Ok(blocked.clone());
        }
        let snapshot = self.repository.get_schema_metadata()?;
        let blocked = Self::classify_blocked_tables(&snapshot);
        *cache = Some(blocked.clone());
        Ok(blocked)
    }

    pub fn schema_context(&self) -> Result<String, QueryRepositoryError> {
        let mut cache = self
            .schema_cache
            .lock()
            .map_err(|_| QueryRepositoryError::Storage("schema cache poisoned".to_owned()))?;
        if let Some(context) = &*cache {
            return Ok(context.clone());
        }
        let snapshot = self.repository.get_schema_metadata()?;
        let blocked = Self::classify_blocked_tables(&snapshot);
        {
            let mut blocked_cache = self.blocked_tables_cache.lock().map_err(|_| {
                QueryRepositoryError::Storage("blocked-table cache poisoned".to_owned())
            })?;
            *blocked_cache = Some(blocked.clone());
        }
        let blocked_columns = BLOCKED_COLUMNS.iter().copied().collect::<BTreeSet<_>>();
        let mut lines = vec!["## Available Tables\n".to_owned()];
        let mut tables = snapshot
            .tables
            .iter()
            .filter(|table| !blocked.contains(&table.name.to_ascii_lowercase()))
            .collect::<Vec<_>>();
        tables.sort_by(|left, right| left.name.cmp(&right.name));
        for table in &tables {
            let columns = table
                .columns
                .iter()
                .filter(|column| {
                    !blocked_columns.contains(column.name.to_ascii_lowercase().as_str())
                })
                .collect::<Vec<_>>();
            if columns.is_empty() {
                continue;
            }
            lines.push(format!("### {}", table.name));
            for column in columns {
                let data_type = if column.data_type.is_empty() {
                    "ANY"
                } else {
                    &column.data_type
                };
                let primary_key = if column.primary_key { " PK" } else { "" };
                let nullable = if column.not_null { "" } else { " (nullable)" };
                lines.push(format!(
                    "  - {}: {data_type}{primary_key}{nullable}",
                    column.name
                ));
            }
            lines.push(String::new());
        }
        lines.push("## Relationships (use for JOINs, don't SELECT these ID columns)".to_owned());
        let relationships = tables
            .into_iter()
            .flat_map(|table| {
                table
                    .foreign_keys
                    .iter()
                    .filter(|foreign_key| {
                        !blocked.contains(&foreign_key.table.to_ascii_lowercase())
                    })
                    .map(|foreign_key| {
                        format!(
                            "- {}.{} = {}.{}",
                            table.name, foreign_key.from, foreign_key.table, foreign_key.to
                        )
                    })
            })
            .collect::<BTreeSet<_>>();
        lines.extend(relationships);
        let context = lines.join("\n");
        *cache = Some(context.clone());
        Ok(context)
    }

    pub fn validate_sql(&self, sql: &str) -> Result<(), SqlValidationError> {
        if sql.trim().is_empty() {
            return Err(SqlValidationError("Empty query".to_owned()));
        }
        let masked = mask_string_literals(sql);
        let lower = masked.to_ascii_lowercase();
        if lower.contains("--") || lower.contains("/*") {
            return Err(SqlValidationError(
                "SQL comments are not allowed".to_owned(),
            ));
        }
        if !lower.trim_start().starts_with("select") {
            return Err(SqlValidationError(
                "Only SELECT queries are allowed".to_owned(),
            ));
        }
        let words = lex_sql(&lower)
            .into_iter()
            .filter_map(|token| token.word().map(str::to_owned))
            .collect::<BTreeSet<_>>();
        if let Some(keyword) = DANGEROUS_KEYWORDS
            .iter()
            .find(|keyword| words.contains(**keyword))
        {
            return Err(SqlValidationError(format!(
                "Forbidden keyword: {}",
                keyword.to_ascii_uppercase()
            )));
        }
        if masked
            .split(';')
            .filter(|part| !part.trim().is_empty())
            .count()
            > 1
        {
            return Err(SqlValidationError(
                "Multiple statements not allowed".to_owned(),
            ));
        }
        if wildcard_projection(&lower) {
            return Err(SqlValidationError(
                "SELECT * is not allowed; list explicit columns".to_owned(),
            ));
        }
        if schema_qualified_reference(&lower) {
            return Err(SqlValidationError(
                "Schema-qualified table references are not allowed".to_owned(),
            ));
        }
        let blocked_tables = self
            .blocked_table_names()
            .map_err(|_| SqlValidationError("Could not verify table guild scoping".to_owned()))?;
        for table in extract_tables(&lower) {
            if table.starts_with("sqlite_")
                || table.starts_with("pragma_")
                || blocked_tables.contains(&table)
            {
                return Err(SqlValidationError(format!("Table not allowed: {table}")));
            }
        }
        let blocked = blocked_projection_columns(&lower);
        if !blocked.is_empty() {
            return Err(SqlValidationError(format!(
                "Blocked column(s): {}",
                blocked.into_iter().collect::<Vec<_>>().join(", ")
            )));
        }
        Ok(())
    }

    #[must_use]
    pub fn query(
        &self,
        ai: &dyn SqlGenerationPort,
        config: Option<&dyn GuildAiConfigPort>,
        guild_id: Option<i64>,
        question: &str,
        asker_discord_id: Option<i64>,
    ) -> QueryResult {
        if let (Some(guild_id), Some(config)) = (guild_id, config)
            && !matches!(config.ai_enabled(guild_id), Ok(true))
        {
            return QueryResult {
                error: Some(
                    "AI features are not enabled for this server. An admin can enable them."
                        .to_owned(),
                ),
                ..QueryResult::default()
            };
        }
        let schema_context = match self.schema_context() {
            Ok(context) => context,
            Err(error) => {
                return QueryResult {
                    error: Some(error.to_string()),
                    ..QueryResult::default()
                };
            }
        };
        let asker_username = asker_discord_id.and_then(|discord_id| {
            self.repository
                .execute_readonly_guild_scoped(
                    "SELECT discord_username FROM players WHERE discord_id = ?",
                    guild_id,
                    &[Value::Integer(discord_id)],
                    1,
                )
                .ok()
                .and_then(|rows| rows.into_iter().next())
                .and_then(|row| {
                    row.get("discord_username")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
        });
        let Some(generated) = ai.generate_sql(&SqlGenerationRequest {
            question: question.to_owned(),
            schema_context,
            asker_discord_id,
            asker_username,
        }) else {
            return QueryResult {
                error: Some("Failed to generate SQL query".to_owned()),
                ..QueryResult::default()
            };
        };
        if let Err(error) = self.validate_sql(&generated.sql) {
            return QueryResult {
                sql: Some(generated.sql),
                error: Some(format!("Query validation failed: {error}")),
                ..QueryResult::default()
            };
        }
        match self
            .repository
            .execute_readonly_guild_scoped(&generated.sql, guild_id, &[], 25)
        {
            Ok(results) => QueryResult {
                success: true,
                sql: Some(generated.sql),
                explanation: Some(generated.explanation),
                row_count: results.len(),
                results,
                error: None,
            },
            Err(error) => QueryResult {
                sql: Some(generated.sql),
                error: Some(format!("Query execution failed: {error}")),
                ..QueryResult::default()
            },
        }
    }
}

pub const BETTING_LAST_CALL_EXAMPLES: [&str; 5] = [
    "Last call — the pool's bleeding out and half of you are just watching. Bet.",
    "Sixty seconds. The odds won't wait, and neither will the winners.",
    "Window's closing. Fortune favors the bold and forgets the hesitant.",
    "Final call to get your jopacoin down. Don't say the bookie didn't warn you.",
    "One minute left. Bet now or spectate the payouts you could've had.",
];

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FlavorEvent {
    LoanTaken,
    NegativeLoan,
    LoanCooldown,
    LoanDeniedDebt,
    BankruptcyDeclared,
    BankruptcyCooldown,
    DebtPaid,
    BetWon,
    BetLost,
    LeverageLoss,
    MatchWin,
    ShopAnnounce,
    ShopAnnounceTarget,
    DoubleOrNothingWin,
    DoubleOrNothingLose,
    DoubleOrNothingZero,
    MvpCallout,
    BetLastCall,
}

pub const ALL_FLAVOR_EVENTS: [FlavorEvent; 18] = [
    FlavorEvent::LoanTaken,
    FlavorEvent::NegativeLoan,
    FlavorEvent::LoanCooldown,
    FlavorEvent::LoanDeniedDebt,
    FlavorEvent::BankruptcyDeclared,
    FlavorEvent::BankruptcyCooldown,
    FlavorEvent::DebtPaid,
    FlavorEvent::BetWon,
    FlavorEvent::BetLost,
    FlavorEvent::LeverageLoss,
    FlavorEvent::MatchWin,
    FlavorEvent::ShopAnnounce,
    FlavorEvent::ShopAnnounceTarget,
    FlavorEvent::DoubleOrNothingWin,
    FlavorEvent::DoubleOrNothingLose,
    FlavorEvent::DoubleOrNothingZero,
    FlavorEvent::MvpCallout,
    FlavorEvent::BetLastCall,
];

impl FlavorEvent {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LoanTaken => "loan_taken",
            Self::NegativeLoan => "negative_loan",
            Self::LoanCooldown => "loan_cooldown",
            Self::LoanDeniedDebt => "loan_denied_debt",
            Self::BankruptcyDeclared => "bankruptcy_declared",
            Self::BankruptcyCooldown => "bankruptcy_cooldown",
            Self::DebtPaid => "debt_paid",
            Self::BetWon => "bet_won",
            Self::BetLost => "bet_lost",
            Self::LeverageLoss => "leverage_loss",
            Self::MatchWin => "match_win",
            Self::ShopAnnounce => "shop_announce",
            Self::ShopAnnounceTarget => "shop_announce_target",
            Self::DoubleOrNothingWin => "double_or_nothing_win",
            Self::DoubleOrNothingLose => "double_or_nothing_lose",
            Self::DoubleOrNothingZero => "double_or_nothing_zero",
            Self::MvpCallout => "mvp_callout",
            Self::BetLastCall => "bet_last_call",
        }
    }

    #[must_use]
    pub const fn examples(self) -> &'static [&'static str] {
        match self {
            Self::LoanTaken => &["Your financial advisor just felt a disturbance in the force."],
            Self::NegativeLoan => &["Borrowing while broke is financial performance art."],
            Self::LoanCooldown => &["The ink on your last loan is not even dry."],
            Self::LoanDeniedDebt => &["Even loan sharks have standards."],
            Self::BankruptcyDeclared => &["Financial reset complete. Shame meter: still maxed."],
            Self::BankruptcyCooldown => &["The bankruptcy judge is still laughing."],
            Self::DebtPaid => &["Debt-free! Quick, take out another loan."],
            Self::BetWon => &["Even a broken clock is right twice a day."],
            Self::BetLost => &["The house sends its regards — and kept your money."],
            Self::LeverageLoss => &["High risk, no reward. Classic."],
            Self::MatchWin => &["The throne just heard footsteps."],
            Self::ShopAnnounce => &["Questionable financial priorities have entered the chat."],
            Self::ShopAnnounceTarget => &["The numbers say get rekt."],
            Self::DoubleOrNothingWin => &["The coin gods smile today."],
            Self::DoubleOrNothingLose => &["And it is gone. Every last jopacoin."],
            Self::DoubleOrNothingZero => &["Zero doubled is still zero."],
            Self::MvpCallout => &["The lobby was a stage. They were the show."],
            Self::BetLastCall => &BETTING_LAST_CALL_EXAMPLES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerRecord {
    pub username: String,
    pub balance: i64,
    pub wins: i64,
    pub losses: i64,
    pub lowest_balance: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerContext {
    pub username: String,
    pub balance: i64,
    pub debt_amount: Option<i64>,
    pub win_rate: Option<f64>,
    pub degen_score: Option<i64>,
    pub bankruptcy_count: i64,
    pub total_bets: i64,
    pub total_loans: i64,
    pub negative_loans: i64,
    pub total_fees_paid: i64,
    pub biggest_win: Option<i64>,
    pub biggest_loss: Option<i64>,
    pub bet_win_rate: Option<f64>,
    pub times_in_debt: i64,
    pub lowest_balance: Option<i64>,
}

impl PlayerContext {
    #[must_use]
    pub fn from_record(record: PlayerRecord) -> Self {
        let games = record.wins.saturating_add(record.losses);
        Self {
            username: record.username,
            balance: record.balance,
            debt_amount: (record.balance < 0).then(|| record.balance.saturating_abs()),
            win_rate: (games > 0).then(|| record.wins as f64 / games as f64 * 100.0),
            degen_score: None,
            bankruptcy_count: 0,
            total_bets: 0,
            total_loans: 0,
            negative_loans: 0,
            total_fees_paid: 0,
            biggest_win: None,
            biggest_loss: None,
            bet_win_rate: None,
            times_in_debt: 0,
            lowest_balance: record.lowest_balance,
        }
    }

    #[must_use]
    pub fn prompt_context(&self) -> BTreeMap<String, Value> {
        let mut context = BTreeMap::from([
            ("username".to_owned(), Value::Text(self.username.clone())),
            ("balance".to_owned(), Value::Integer(self.balance)),
            ("total_bets".to_owned(), Value::Integer(self.total_bets)),
            ("total_loans".to_owned(), Value::Integer(self.total_loans)),
            (
                "negative_loans".to_owned(),
                Value::Integer(self.negative_loans),
            ),
            (
                "bankruptcy_count".to_owned(),
                Value::Integer(self.bankruptcy_count),
            ),
            (
                "total_fees_paid".to_owned(),
                Value::Integer(self.total_fees_paid),
            ),
            (
                "times_in_debt".to_owned(),
                Value::Integer(self.times_in_debt),
            ),
        ]);
        context.insert(
            "debt_amount".to_owned(),
            self.debt_amount.map_or(Value::Null, Value::Integer),
        );
        context.insert(
            "win_rate".to_owned(),
            self.win_rate.map_or_else(
                || Value::Text("Unknown".to_owned()),
                |value| Value::Text(format!("{value:.0}%")),
            ),
        );
        context.insert(
            "degen_score".to_owned(),
            self.degen_score.map_or(Value::Null, Value::Integer),
        );
        context.insert(
            "biggest_win".to_owned(),
            self.biggest_win.map_or(Value::Null, Value::Integer),
        );
        context.insert(
            "biggest_loss".to_owned(),
            self.biggest_loss.map_or(Value::Null, Value::Integer),
        );
        // Preserve an observed zero win rate as "0%". Python's betting
        // payload distinguishes it from an absent stats row.
        context.insert(
            "bet_win_rate".to_owned(),
            self.bet_win_rate
                .map_or(Value::Null, |value| Value::Text(format!("{value:.0}%"))),
        );
        context.insert(
            "lowest_balance".to_owned(),
            self.lowest_balance.map_or(Value::Null, Value::Integer),
        );
        context
    }
}

pub trait PlayerContextPort: Send + Sync {
    fn player_context(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<PlayerContext>, String>;
}

pub trait FlavorAiPort: Send + Sync {
    fn generate_flavor(&self, input: &FlavorPromptInput) -> Result<Option<String>, String>;
    fn complete_insight(
        &self,
        prompt: &str,
        system_prompt: &str,
        feature: &str,
    ) -> Result<Option<String>, String>;
}

impl FlavorAiPort for AIService {
    fn generate_flavor(&self, input: &FlavorPromptInput) -> Result<Option<String>, String> {
        Ok(AIService::generate_flavor(self, input))
    }

    fn complete_insight(
        &self,
        prompt: &str,
        system_prompt: &str,
        feature: &str,
    ) -> Result<Option<String>, String> {
        Ok(self.complete(prompt, Some(system_prompt), 0.7, Some(2_000), feature))
    }
}

pub struct FlavorTextService {
    ai: Arc<dyn FlavorAiPort>,
    players: Arc<dyn PlayerContextPort>,
    config: Option<Arc<dyn GuildAiConfigPort>>,
    betting_random: Arc<dyn BettingFlavorRandom>,
}

impl FlavorTextService {
    #[must_use]
    pub fn new(ai: Arc<dyn FlavorAiPort>, players: Arc<dyn PlayerContextPort>) -> Self {
        Self {
            ai,
            players,
            config: None,
            betting_random: Arc::new(ThreadBettingFlavorRandom),
        }
    }

    #[must_use]
    pub fn with_config(mut self, config: Arc<dyn GuildAiConfigPort>) -> Self {
        self.config = Some(config);
        self
    }

    #[must_use]
    pub fn with_betting_random(mut self, random: Arc<dyn BettingFlavorRandom>) -> Self {
        self.betting_random = random;
        self
    }

    fn ai_enabled(&self, guild_id: Option<i64>) -> bool {
        match (guild_id, &self.config) {
            (Some(guild_id), Some(config)) => matches!(config.ai_enabled(guild_id), Ok(true)),
            _ => true,
        }
    }

    fn fallback(event: FlavorEvent) -> Option<String> {
        event.examples().first().map(|value| (*value).to_owned())
    }

    #[must_use]
    pub fn generate_event_flavor(
        &self,
        guild_id: Option<i64>,
        event: FlavorEvent,
        discord_id: i64,
        event_details: BTreeMap<String, Value>,
    ) -> Option<String> {
        if !self.ai_enabled(guild_id) {
            return Self::fallback(event);
        }
        let context = match self.players.player_context(discord_id, guild_id) {
            Ok(Some(context)) => context,
            _ => return Self::fallback(event),
        };
        let input = FlavorPromptInput {
            event_type: event.as_str().to_owned(),
            player_context: context.prompt_context(),
            event_details,
            examples: event
                .examples()
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            persona: None,
        };
        match self.ai.generate_flavor(&input) {
            Ok(Some(value)) => sanitize_output(Some(&value), 220).or_else(|| Self::fallback(event)),
            _ => Self::fallback(event),
        }
    }

    #[must_use]
    pub fn generate_betting_last_call(
        &self,
        guild_id: Option<i64>,
        mut event_details: BTreeMap<String, Value>,
    ) -> Option<String> {
        self.generate_betting_line(guild_id, "bet_last_call", &mut event_details)
    }

    #[must_use]
    pub fn generate_betting_warning(
        &self,
        guild_id: Option<i64>,
        mut event_details: BTreeMap<String, Value>,
    ) -> Option<String> {
        self.generate_betting_line(guild_id, "bet_warning", &mut event_details)
    }

    fn betting_fallback(&self) -> Option<String> {
        let index = self
            .betting_random
            .choose_index(BETTING_LAST_CALL_EXAMPLES.len());
        BETTING_LAST_CALL_EXAMPLES
            .get(index)
            .map(|value| (*value).to_owned())
    }

    fn generate_betting_line(
        &self,
        guild_id: Option<i64>,
        event_type: &str,
        event_details: &mut BTreeMap<String, Value>,
    ) -> Option<String> {
        if !self.ai_enabled(guild_id) {
            return self.betting_fallback();
        }
        let leader_id = event_details
            .get("leader_discord_id")
            .and_then(Value::as_i64);
        // The ID is an internal lookup key; Python never sends it to the LLM.
        event_details.remove("leader_discord_id");
        let has_bettor = leader_id.is_some();
        let underdog_side = event_details
            .get("underdog_side")
            .and_then(Value::as_str)
            .filter(|side| !side.is_empty());
        let angle = if event_type == "bet_warning" {
            if underdog_side.is_some() {
                ["roast_underdog", "hype_underdog"][self.betting_random.choose_index(2)]
            } else if has_bettor {
                ["taunt_crowd", "roast_leader", "hype_leader"][self.betting_random.choose_index(3)]
            } else {
                "taunt_crowd"
            }
        } else if has_bettor {
            ["taunt_crowd", "roast_leader", "hype_leader"][self.betting_random.choose_index(3)]
        } else {
            "taunt_crowd"
        };
        event_details.insert("angle".to_owned(), Value::Text(angle.to_owned()));
        event_details.insert("has_bettor".to_owned(), Value::Bool(has_bettor));
        if event_type == "bet_warning" {
            event_details
                .entry("underdog_side".to_owned())
                .or_insert(Value::Null);
        }
        let mut player_context = BTreeMap::new();
        if let Some(leader_id) = leader_id
            && let Ok(Some(context)) = self.players.player_context(leader_id, guild_id)
        {
            let username = context.username.clone();
            player_context = context.prompt_context();
            event_details
                .entry("leader_name".to_owned())
                .or_insert(Value::Text(username));
        }
        let persona = betting_personas()
            .get(self.betting_random.choose_index(betting_personas().len()))
            .cloned();
        let examples = persona
            .as_ref()
            .map_or_else(Vec::new, |persona| persona.examples.clone());
        let input = FlavorPromptInput {
            event_type: event_type.to_owned(),
            player_context,
            event_details: event_details.clone(),
            examples,
            persona,
        };
        match self.ai.generate_flavor(&input) {
            Ok(Some(value)) => {
                sanitize_output(Some(&value), 220).or_else(|| self.betting_fallback())
            }
            _ => self.betting_fallback(),
        }
    }

    #[must_use]
    pub fn generate_data_insight(
        &self,
        guild_id: Option<i64>,
        data_type: &str,
        data: &BTreeMap<String, Value>,
        context: Option<&str>,
    ) -> Option<String> {
        if !self.ai_enabled(guild_id) {
            return None;
        }
        let prompt = build_insight_prompt(data_type, data, context)?;
        let system_prompt = "You are a helpful analyst for a Dota 2 inhouse league. Generate a \
                             brief summary focused on key takeaways.";
        self.ai
            .complete_insight(
                &prompt,
                system_prompt,
                &format!("stats.{data_type}_insight"),
            )
            .ok()
            .flatten()
            .and_then(|value| sanitize_output(Some(&value), 1_000))
    }
}

impl crate::betting_reminder_messaging::BettingFlavorPort for FlavorTextService {
    fn generate(
        &self,
        request: &crate::betting_reminder_messaging::FlavorRequest,
    ) -> Result<Option<String>, String> {
        let mut details = BTreeMap::from([
            (
                "standings".to_owned(),
                Value::Text(request.standings.clone()),
            ),
            (
                "seconds_left".to_owned(),
                Value::Integer(request.seconds_left),
            ),
            (
                "leader_discord_id".to_owned(),
                request
                    .leader_discord_id
                    .map_or(Value::Null, Value::Integer),
            ),
            (
                "leader_amount".to_owned(),
                request.leader_amount.map_or(Value::Null, Value::Integer),
            ),
            (
                "leader_team".to_owned(),
                request.leader_team.clone().map_or(Value::Null, Value::Text),
            ),
        ]);
        if let Some(underdog) = request.underdog_side {
            details.insert(
                "underdog_side".to_owned(),
                Value::Text(underdog.as_str().to_owned()),
            );
        } else {
            details.insert("underdog_side".to_owned(), Value::Null);
        }
        let event_type = match request.kind {
            crate::betting_reminder_messaging::FlavorKind::Warning => "bet_warning",
            crate::betting_reminder_messaging::FlavorKind::LastCall => "bet_last_call",
        };
        Ok(match event_type {
            "bet_warning" => self.generate_betting_warning(request.guild_id, details),
            _ => self.generate_betting_last_call(request.guild_id, details),
        })
    }
}

fn render_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::Text(value)) => value.clone(),
        Some(Value::Integer(value)) => value.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Real(value)) => value.to_string(),
        Some(Value::List(values)) => format!("{values:?}"),
        Some(Value::Object(value)) => format!("{value:?}"),
        Some(Value::Null) | None => String::new(),
    }
}

fn build_insight_prompt(
    data_type: &str,
    data: &BTreeMap<String, Value>,
    context: Option<&str>,
) -> Option<String> {
    let mut prompt = match data_type {
        "pairwise" => format!(
            "Pairwise stats for {}:",
            render_value(data.get("player_name")).trim()
        ),
        "matchup" => format!(
            "Head-to-head: {} vs {}",
            render_value(data.get("player1_name")),
            render_value(data.get("player2_name"))
        ),
        "leaderboard" => format!(
            "Leaderboard with {} players: {}",
            render_value(data.get("total_players")),
            render_value(data.get("top_players"))
        ),
        _ => return None,
    };
    if let Some(context) = context {
        prompt.push_str("\nAdditional context: ");
        prompt.push_str(context);
    }
    prompt.push_str("\nExplain the notable patterns and key takeaways.");
    Some(prompt)
}

#[must_use]
pub fn contains_link_or_markup(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if lower.contains("://")
        || ["data:", "javascript:", "mailto:", "sms:", "tel:"]
            .iter()
            .any(|scheme| lower.contains(scheme))
        || (lower.contains('[') && lower.contains("]("))
        || ["<@", "<#", "</", "<t:", "<:", "<a:"]
            .iter()
            .any(|prefix| lower.contains(prefix))
    {
        return true;
    }
    lower.split_whitespace().any(|token| {
        let token = token.trim_matches(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '.' | ':' | '/' | '-')
        });
        let host = token.split(['/', ':']).next().unwrap_or(token);
        let mut labels = host.split('.');
        let first = labels.next();
        let rest = labels.collect::<Vec<_>>();
        let domain = first.is_some()
            && !rest.is_empty()
            && rest.last().is_some_and(|suffix| {
                suffix.len() >= 2
                    && suffix
                        .chars()
                        .all(|character| character.is_ascii_alphabetic())
            });
        let ipv4 = host.split('.').count() == 4
            && host.split('.').all(|octet| octet.parse::<u8>().is_ok());
        domain || ipv4
    })
}

#[must_use]
pub fn sanitize_output(value: Option<&str>, max_chars: usize) -> Option<String> {
    let value = value?;
    let cleaned = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect::<String>();
    if cleaned.is_empty() || contains_link_or_markup(&cleaned) {
        return None;
    }
    Some(cleaned.replace('@', "＠"))
}

#[cfg(test)]
#[path = "ai_services/tests.rs"]
mod tests;
