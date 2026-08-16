//! OpenAI-compatible production HTTP transport for the typed AI service.
//!
//! Groq and Cerebras both expose bearer-authenticated Chat Completions APIs.
//! This adapter keeps that network boundary separate from prompt and fallback
//! policy in [`crate::ai_services`], never stores or logs credentials or prompt
//! bodies, enforces the caller's hard timeout, rejects oversized responses, and
//! maps provider failures into the existing fail-soft error taxonomy.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::{Map, Value as JsonValue, json};

use crate::ai_services::{
    AIService, AiProvider, AttemptMetadata, AttemptRecorder, ConfigError, MessageRole,
    ProviderConfig, ProviderCredential, ProviderError, ProviderErrorKind, ProviderLoader,
    ProviderName, ProviderRequest, ProviderResponse, TokenUsage, ToolCall, ToolChoice,
    ToolDefinition, ToolProperty, ToolPropertySchema, Value,
};
use cama_db::llm_request::{LlmAttemptRecord, LlmRequestRepository};

pub const GROQ_CHAT_COMPLETIONS_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
pub const CEREBRAS_CHAT_COMPLETIONS_URL: &str = "https://api.cerebras.ai/v1/chat/completions";
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenAiCompatibleEndpoint {
    pub provider: ProviderName,
    pub url: String,
    pub max_response_bytes: usize,
}

impl OpenAiCompatibleEndpoint {
    #[must_use]
    pub fn for_provider(provider: ProviderName) -> Option<Self> {
        let url = match provider {
            ProviderName::Groq => GROQ_CHAT_COMPLETIONS_URL,
            ProviderName::Cerebras => CEREBRAS_CHAT_COMPLETIONS_URL,
            ProviderName::Other(_) | ProviderName::Unknown => return None,
        };
        Some(Self {
            provider,
            url: url.to_owned(),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        })
    }

    #[must_use]
    pub fn custom(provider: ProviderName, url: impl Into<String>) -> Self {
        Self {
            provider,
            url: url.into(),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    fn wire_model<'a>(&self, model: &'a str) -> &'a str {
        let expected_prefix = format!("{}/", self.provider.as_str());
        model.strip_prefix(&expected_prefix).unwrap_or(model)
    }
}

#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    client: Client,
    endpoint: OpenAiCompatibleEndpoint,
}

impl fmt::Debug for OpenAiCompatibleProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiCompatibleProvider")
            .field("provider", &self.endpoint.provider)
            .field("url", &self.endpoint.url)
            .field("max_response_bytes", &self.endpoint.max_response_bytes)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatibleProvider {
    pub fn new(endpoint: OpenAiCompatibleEndpoint) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .https_only(endpoint.url.starts_with("https://"))
            .build()
            .map_err(classify_reqwest_error)?;
        Ok(Self { client, endpoint })
    }

    fn send(&self, request: &ProviderRequest) -> Result<Response, ProviderError> {
        let body = request_body(&self.endpoint, request);
        self.client
            .post(&self.endpoint.url)
            .timeout(request.timeout)
            .header(CONTENT_TYPE, "application/json")
            .header(
                AUTHORIZATION,
                format!("Bearer {}", request.credential.expose_to_provider()),
            )
            .json(&body)
            .send()
            .map_err(classify_reqwest_error)
    }

    fn parse_response(&self, response: Response) -> Result<ProviderResponse, ProviderError> {
        let status = response.status();
        if !status.is_success() {
            return Err(classify_status(status));
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.endpoint.max_response_bytes as u64)
        {
            return Err(invalid_response(
                "provider response exceeded the byte limit",
            ));
        }

        let mut body = Vec::new();
        response
            .take((self.endpoint.max_response_bytes + 1) as u64)
            .read_to_end(&mut body)
            .map_err(|_| invalid_response("provider response body could not be read"))?;
        if body.len() > self.endpoint.max_response_bytes {
            return Err(invalid_response(
                "provider response exceeded the byte limit",
            ));
        }
        let payload: JsonValue = serde_json::from_slice(&body)
            .map_err(|_| invalid_response("provider returned malformed JSON"))?;
        parse_provider_response(&payload)
    }
}

impl AiProvider for OpenAiCompatibleProvider {
    fn complete(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        let response = self.send(&request)?;
        self.parse_response(response)
    }
}

#[derive(Clone, Debug)]
pub struct OpenAiCompatibleProviderLoader {
    endpoint: OpenAiCompatibleEndpoint,
}

impl OpenAiCompatibleProviderLoader {
    #[must_use]
    pub const fn new(endpoint: OpenAiCompatibleEndpoint) -> Self {
        Self { endpoint }
    }
}

impl ProviderLoader for OpenAiCompatibleProviderLoader {
    fn load(&self) -> Result<Arc<dyn AiProvider>, ProviderError> {
        OpenAiCompatibleProvider::new(self.endpoint.clone())
            .map(|provider| Arc::new(provider) as Arc<dyn AiProvider>)
    }
}

#[derive(Clone, Debug)]
pub struct SqliteAttemptRecorder {
    repository: LlmRequestRepository,
}

impl SqliteAttemptRecorder {
    #[must_use]
    pub fn new(path: impl AsRef<std::path::Path>) -> Self {
        Self {
            repository: LlmRequestRepository::new(path),
        }
    }
}

impl AttemptRecorder for SqliteAttemptRecorder {
    fn record_attempt(&self, attempt: AttemptMetadata) -> Result<(), String> {
        let latency_ms = (attempt.latency.as_secs_f64() * 1_000.0).round();
        let latency_ms = if latency_ms >= i64::MAX as f64 {
            i64::MAX
        } else {
            latency_ms as i64
        };
        self.repository
            .record_attempt(&LlmAttemptRecord {
                feature: attempt.feature,
                operation: attempt.operation.as_str().to_owned(),
                provider: attempt.provider,
                model: attempt.model,
                success: attempt.success,
                latency_ms,
                prompt_tokens: attempt.usage.prompt_tokens.map(saturating_i64),
                completion_tokens: attempt.usage.completion_tokens.map(saturating_i64),
                total_tokens: attempt.usage.total_tokens.map(saturating_i64),
                error_type: attempt.error_kind.map(|kind| format!("{kind:?}")),
            })
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

pub fn production_ai_service(
    config: ProviderConfig,
    database_path: impl AsRef<std::path::Path>,
) -> Result<Arc<AIService>, ProviderError> {
    let endpoint =
        OpenAiCompatibleEndpoint::for_provider(config.provider.clone()).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Loader,
                format!(
                    "no production HTTP endpoint is configured for {}",
                    config.provider.as_str()
                ),
            )
        })?;
    let loader = Arc::new(OpenAiCompatibleProviderLoader::new(endpoint));
    let recorder = Arc::new(SqliteAttemptRecorder::new(database_path));
    Ok(Arc::new(
        AIService::new(config, loader).with_attempt_recorder(recorder),
    ))
}

pub fn production_ai_service_from_settings(
    model: impl Into<String>,
    credential: impl Into<String>,
    timeout_seconds: f64,
    max_tokens: i64,
    database_path: impl AsRef<std::path::Path>,
) -> Result<Arc<AIService>, ProductionAiBuildError> {
    let timeout = Duration::try_from_secs_f64(timeout_seconds)
        .map_err(|_| ProductionAiBuildError::InvalidTimeout(timeout_seconds))?;
    let max_tokens = u32::try_from(max_tokens)
        .map_err(|_| ProductionAiBuildError::InvalidMaxTokens(max_tokens))?;
    let credential = ProviderCredential::new(credential.into())?;
    let config = ProviderConfig::new(model, credential, timeout, max_tokens)?;
    production_ai_service(config, database_path).map_err(Into::into)
}

#[derive(Debug, thiserror::Error)]
pub enum ProductionAiBuildError {
    #[error("invalid AI provider timeout {0:?}")]
    InvalidTimeout(f64),
    #[error("invalid AI provider max token count {0}")]
    InvalidMaxTokens(i64),
    #[error("invalid AI provider configuration: {0}")]
    Config(#[from] ConfigError),
    #[error("AI provider construction failed: {0}")]
    Provider(#[from] ProviderError),
}

fn saturating_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn request_body(endpoint: &OpenAiCompatibleEndpoint, request: &ProviderRequest) -> JsonValue {
    let messages = request
        .messages
        .iter()
        .map(|message| {
            json!({
                "role": role_name(message.role),
                "content": message.content,
            })
        })
        .collect::<Vec<_>>();
    let mut body = Map::from_iter([
        (
            "model".to_owned(),
            JsonValue::String(endpoint.wire_model(&request.model).to_owned()),
        ),
        ("messages".to_owned(), JsonValue::Array(messages)),
        ("max_tokens".to_owned(), JsonValue::from(request.max_tokens)),
    ]);
    if let Some(temperature) = request.temperature {
        body.insert("temperature".to_owned(), JsonValue::from(temperature));
    }
    if !request.tools.is_empty() {
        body.insert(
            "tools".to_owned(),
            JsonValue::Array(request.tools.iter().map(tool_body).collect()),
        );
    }
    if request.operation == crate::ai_services::AiOperation::ToolCall {
        body.insert(
            "tool_choice".to_owned(),
            tool_choice_body(&request.tool_choice),
        );
    }
    if let Some(reasoning_format) = &request.options.reasoning_format {
        body.insert(
            "reasoning_format".to_owned(),
            JsonValue::String(reasoning_format.clone()),
        );
    }
    if let Some(reasoning_effort) = &request.options.reasoning_effort {
        body.insert(
            "reasoning_effort".to_owned(),
            JsonValue::String(reasoning_effort.clone()),
        );
    }
    if let Some(parallel_tool_calls) = request.options.parallel_tool_calls {
        body.insert(
            "parallel_tool_calls".to_owned(),
            JsonValue::Bool(parallel_tool_calls),
        );
    }
    JsonValue::Object(body)
}

const fn role_name(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn tool_body(tool: &ToolDefinition) -> JsonValue {
    let properties = tool
        .properties
        .iter()
        .map(tool_property_body)
        .collect::<Map<_, _>>();
    let mut parameters = Map::from_iter([
        ("type".to_owned(), JsonValue::String("object".to_owned())),
        ("properties".to_owned(), JsonValue::Object(properties)),
        ("required".to_owned(), json!(tool.required)),
    ]);
    if let Some(additional_properties) = tool.additional_properties {
        parameters.insert(
            "additionalProperties".to_owned(),
            JsonValue::Bool(additional_properties),
        );
    }
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": parameters,
        },
    })
}

/// Serialize one property without the outer tool-property name.
///
/// Keeping this helper separate makes nested object schemas use exactly the
/// same description, enum, array, number, and recursive-object rules as
/// top-level properties.
fn tool_property_body(property: &ToolProperty) -> (String, JsonValue) {
    let mut schema = Map::new();
    if !property.description.is_empty() {
        schema.insert(
            "description".to_owned(),
            JsonValue::String(property.description.clone()),
        );
    }
    match &property.schema {
        ToolPropertySchema::String => {
            schema.insert("type".to_owned(), JsonValue::String("string".to_owned()));
        }
        ToolPropertySchema::Number => {
            schema.insert("type".to_owned(), JsonValue::String("number".to_owned()));
        }
        ToolPropertySchema::StringArray {
            min_items,
            max_items,
            unique_items,
        } => {
            schema.insert("type".to_owned(), JsonValue::String("array".to_owned()));
            schema.insert("items".to_owned(), json!({ "type": "string" }));
            schema.insert("minItems".to_owned(), JsonValue::from(*min_items));
            schema.insert("maxItems".to_owned(), JsonValue::from(*max_items));
            schema.insert("uniqueItems".to_owned(), JsonValue::from(*unique_items));
        }
        ToolPropertySchema::Object {
            properties,
            required,
            additional_properties,
        } => {
            let nested_properties = properties
                .iter()
                .map(tool_property_body)
                .collect::<Map<_, _>>();
            schema.insert("type".to_owned(), JsonValue::String("object".to_owned()));
            schema.insert(
                "properties".to_owned(),
                JsonValue::Object(nested_properties),
            );
            schema.insert("required".to_owned(), json!(required));
            if let Some(additional_properties) = additional_properties {
                schema.insert(
                    "additionalProperties".to_owned(),
                    JsonValue::Bool(*additional_properties),
                );
            }
        }
    }
    if !property.enum_values.is_empty() {
        schema.insert(
            "enum".to_owned(),
            JsonValue::Array(
                property
                    .enum_values
                    .iter()
                    .cloned()
                    .map(JsonValue::String)
                    .collect(),
            ),
        );
    }
    (property.name.clone(), JsonValue::Object(schema))
}

fn tool_choice_body(choice: &ToolChoice) -> JsonValue {
    match choice {
        ToolChoice::Auto => JsonValue::String("auto".to_owned()),
        ToolChoice::None => JsonValue::String("none".to_owned()),
        ToolChoice::Required(name) => json!({
            "type": "function",
            "function": { "name": name },
        }),
    }
}

fn parse_provider_response(payload: &JsonValue) -> Result<ProviderResponse, ProviderError> {
    let message = payload
        .get("choices")
        .and_then(JsonValue::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(JsonValue::as_object)
        .ok_or_else(|| invalid_response("provider response contained no choice message"))?;

    let content = optional_string(message.get("content"));
    let reasoning_content = optional_string(
        message
            .get("reasoning_content")
            .or_else(|| message.get("reasoning")),
    );
    let tool_calls = message
        .get("tool_calls")
        .and_then(JsonValue::as_array)
        .map_or_else(Vec::new, |calls| {
            calls.iter().filter_map(parse_tool_call).collect()
        });
    let usage = payload
        .get("usage")
        .map_or_else(TokenUsage::default, |usage| TokenUsage {
            prompt_tokens: token_count(usage.get("prompt_tokens")),
            completion_tokens: token_count(usage.get("completion_tokens")),
            total_tokens: token_count(usage.get("total_tokens")),
        });
    Ok(ProviderResponse {
        content,
        reasoning_content,
        tool_calls,
        usage,
    })
}

fn parse_tool_call(call: &JsonValue) -> Option<ToolCall> {
    let function = call.get("function")?;
    let name = function.get("name")?.as_str()?.to_owned();
    let arguments = function
        .get("arguments")
        .and_then(JsonValue::as_str)
        .and_then(|arguments| serde_json::from_str::<JsonValue>(arguments).ok())
        .and_then(|arguments| arguments.as_object().cloned())
        .map_or_else(BTreeMap::new, |arguments| {
            arguments
                .into_iter()
                .map(|(key, value)| (key, convert_value(value)))
                .collect()
        });
    Some(ToolCall { name, arguments })
}

fn convert_value(value: JsonValue) -> Value {
    match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(value) => Value::Bool(value),
        JsonValue::Number(value) => value.as_i64().map_or_else(
            || value.as_f64().map_or(Value::Null, Value::Real),
            Value::Integer,
        ),
        JsonValue::String(value) => Value::Text(value),
        JsonValue::Array(values) => Value::List(values.into_iter().map(convert_value).collect()),
        JsonValue::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, convert_value(value)))
                .collect(),
        ),
    }
}

fn optional_string(value: Option<&JsonValue>) -> Option<String> {
    value.and_then(JsonValue::as_str).map(str::to_owned)
}

fn token_count(value: Option<&JsonValue>) -> Option<u64> {
    value.and_then(|value| {
        value.as_u64().or_else(|| {
            value
                .as_f64()
                .filter(|value| *value >= 0.0)
                .map(|value| value as u64)
        })
    })
}

fn classify_reqwest_error(error: reqwest::Error) -> ProviderError {
    let kind = if error.is_timeout() {
        ProviderErrorKind::Timeout
    } else if error.is_connect() {
        ProviderErrorKind::Unavailable
    } else {
        ProviderErrorKind::Other
    };
    ProviderError::new(kind, "OpenAI-compatible provider request failed")
}

fn classify_status(status: StatusCode) -> ProviderError {
    let kind = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderErrorKind::Authentication,
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => ProviderErrorKind::Timeout,
        StatusCode::TOO_MANY_REQUESTS => ProviderErrorKind::RateLimit,
        status if status.is_server_error() => ProviderErrorKind::Unavailable,
        _ => ProviderErrorKind::InvalidResponse,
    };
    ProviderError::new(kind, format!("provider returned HTTP {status}"))
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests;
