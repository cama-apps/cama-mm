//! Discord-independent `/ask` validation, result formatting, and response policy.

use std::sync::Mutex;

use crate::ai_services::{QueryResult, QueryRow, Value};

pub const MIN_QUESTION_LENGTH: usize = 5;
pub const MAX_QUESTION_LENGTH: usize = 500;
pub const EMBED_QUESTION_LENGTH: usize = 256;
pub const EMBED_ANSWER_LENGTH: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AskConfig {
    pub rate_limit_requests: u32,
    pub rate_limit_window_seconds: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RateLimitRequest {
    pub guild_id: i64,
    pub user_id: i64,
    pub limit: u32,
    pub per_seconds: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateLimitDecision {
    pub allowed: bool,
    pub retry_after_seconds: f64,
}

pub trait AskRateLimiter {
    fn check(&self, request: RateLimitRequest) -> RateLimitDecision;
}

pub trait AskQueryPort {
    fn query(&self, guild_id: i64, question: &str, asker_discord_id: i64) -> QueryResult;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbedColor {
    Blue,
    Red,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AskEmbed {
    pub title: String,
    pub color: EmbedColor,
    pub fields: Vec<EmbedField>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AskResponse {
    ImmediateText { content: String, ephemeral: bool },
    DeferredText { content: String, ephemeral: bool },
    DeferredEmbed { embed: AskEmbed },
}

pub struct AskController<'a, R, Q> {
    config: AskConfig,
    rate_limiter: &'a R,
    query_port: &'a Q,
}

impl<'a, R: AskRateLimiter, Q: AskQueryPort> AskController<'a, R, Q> {
    #[must_use]
    pub const fn new(config: AskConfig, rate_limiter: &'a R, query_port: &'a Q) -> Self {
        Self {
            config,
            rate_limiter,
            query_port,
        }
    }

    #[must_use]
    pub fn handle(&self, guild_id: i64, user_id: i64, question: &str) -> AskResponse {
        let rate_limit = self.rate_limiter.check(RateLimitRequest {
            guild_id,
            user_id,
            limit: self.config.rate_limit_requests,
            per_seconds: self.config.rate_limit_window_seconds,
        });
        if !rate_limit.allowed {
            return AskResponse::ImmediateText {
                content: format!(
                    "Rate limited. Try again in {:.0} seconds.",
                    rate_limit.retry_after_seconds
                ),
                ephemeral: true,
            };
        }

        let question_length = question.chars().count();
        if question_length < MIN_QUESTION_LENGTH {
            return AskResponse::DeferredText {
                content: "Please provide a more detailed question.".to_owned(),
                ephemeral: true,
            };
        }
        if question_length > MAX_QUESTION_LENGTH {
            return AskResponse::DeferredText {
                content: "Question is too long. Please keep it under 500 characters.".to_owned(),
                ephemeral: true,
            };
        }

        let result = self.query_port.query(guild_id, question, user_id);
        let success = result.success;
        let answer = if success {
            format_query_result(&result, EMBED_ANSWER_LENGTH)
        } else {
            "I couldn't answer that. Try asking about player stats, matches, or the leaderboard."
                .to_owned()
        };
        let answer_name = if success { "Answer" } else { "" };
        AskResponse::DeferredEmbed {
            embed: AskEmbed {
                title: "AI Query Result".to_owned(),
                color: if success {
                    EmbedColor::Blue
                } else {
                    EmbedColor::Red
                },
                fields: vec![
                    EmbedField {
                        name: "Question".to_owned(),
                        value: take_chars(question, EMBED_QUESTION_LENGTH),
                        inline: false,
                    },
                    EmbedField {
                        name: answer_name.to_owned(),
                        value: answer,
                        inline: false,
                    },
                ],
            },
        }
    }
}

#[must_use]
pub fn format_query_result(result: &QueryResult, max_length: usize) -> String {
    if !result.success {
        return format!("Error: {}", result.error.as_deref().unwrap_or("None"));
    }
    if result.results.is_empty() {
        return "No results found.".to_owned();
    }

    let show_numbers = result.results.len() > 1;
    let mut lines = result
        .results
        .iter()
        .take(10)
        .enumerate()
        .map(|(index, row)| format_query_row(row, show_numbers.then_some(index + 1)))
        .collect::<Vec<_>>();
    if result.row_count > 10 {
        lines.push(format!("\n*...and {} more*", result.row_count - 10));
    }
    let output = lines.join("\n");
    if output.chars().count() > max_length {
        let prefix_length = max_length.saturating_sub(20);
        format!("{}\n*...truncated*", take_chars(&output, prefix_length))
    } else {
        output
    }
}

fn format_query_row(row: &QueryRow, number: Option<usize>) -> String {
    const NAME_KEYS: [&str; 4] = ["discord_username", "username", "name", "question"];
    let mut parts = Vec::new();
    if let Some(name) = NAME_KEYS
        .iter()
        .find_map(|key| row.get(*key).filter(|value| value_is_truthy(value)))
    {
        parts.push(format!("**{}**", display_value(name)));
    }
    for (key, value) in row {
        if NAME_KEYS.contains(&key.as_str()) || matches!(value, Value::Null) {
            continue;
        }
        let label = humanize_key(key);
        let formatted = match value {
            Value::Real(number)
                if key.to_ascii_lowercase().contains("rate")
                    || key.to_ascii_lowercase().contains("prob") =>
            {
                if *number > 1.0 {
                    format!("{number:.1}% {label}")
                } else {
                    format!("{:.0}% {label}", number * 100.0)
                }
            }
            Value::Real(number) if key.to_ascii_lowercase().contains("rating") => {
                format!("{number:.0} rating")
            }
            Value::Real(number) => format!("{number:.1} {label}"),
            Value::Integer(number) => format!("{number} {label}"),
            Value::Bool(value) => format!("{} {label}", if *value { "True" } else { "False" }),
            other => format!("{label}: {}", display_value(other)),
        };
        parts.push(formatted);
    }
    let content = parts.join(" • ");
    number.map_or(content.clone(), |number| format!("{number}. {content}"))
}

fn value_is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Integer(value) => *value != 0,
        Value::Real(value) => *value != 0.0,
        Value::Text(value) => !value.is_empty(),
        Value::List(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn display_value(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(value) => if *value { "True" } else { "False" }.to_owned(),
        Value::Integer(value) => value.to_string(),
        Value::Real(value) => value.to_string(),
        Value::Text(value) => value.clone(),
        Value::List(values) => format!("{values:?}"),
        Value::Object(values) => format!("{values:?}"),
    }
}

fn humanize_key(key: &str) -> String {
    match key {
        "glicko_rating" => "rating".to_owned(),
        "glicko_rd" => "uncertainty".to_owned(),
        "discord_username" => "player".to_owned(),
        "jopacoin_balance" => "jopacoin".to_owned(),
        "win_rate" => "win rate".to_owned(),
        "games_together" => "games together".to_owned(),
        "games_against" => "games against".to_owned(),
        "wins_together" => "wins together".to_owned(),
        "total_loans_taken" => "loans".to_owned(),
        "total_fees_paid" => "fees paid".to_owned(),
        "outstanding_principal" => "debt".to_owned(),
        other => other.replace('_', " "),
    }
}

fn take_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

#[derive(Default)]
pub struct RecordingRateLimiter {
    pub requests: Mutex<Vec<RateLimitRequest>>,
    pub decision: Mutex<Option<RateLimitDecision>>,
}

impl AskRateLimiter for RecordingRateLimiter {
    fn check(&self, request: RateLimitRequest) -> RateLimitDecision {
        self.requests.lock().expect("rate requests").push(request);
        self.decision
            .lock()
            .expect("rate decision")
            .unwrap_or(RateLimitDecision {
                allowed: true,
                retry_after_seconds: 0.0,
            })
    }
}

#[derive(Default)]
pub struct RecordingQueryPort {
    pub calls: Mutex<Vec<(i64, String, i64)>>,
    pub result: Mutex<QueryResult>,
}

impl AskQueryPort for RecordingQueryPort {
    fn query(&self, guild_id: i64, question: &str, asker_discord_id: i64) -> QueryResult {
        self.calls.lock().expect("query calls").push((
            guild_id,
            question.to_owned(),
            asker_discord_id,
        ));
        self.result.lock().expect("query result").clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn controller<'a>(
        limiter: &'a RecordingRateLimiter,
        query: &'a RecordingQueryPort,
    ) -> AskController<'a, RecordingRateLimiter, RecordingQueryPort> {
        AskController::new(
            AskConfig {
                rate_limit_requests: 3,
                rate_limit_window_seconds: 17,
            },
            limiter,
            query,
        )
    }

    fn row(values: impl IntoIterator<Item = (&'static str, Value)>) -> QueryRow {
        values
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect()
    }

    #[test]
    fn test_too_short_question_rejected() {
        let limiter = RecordingRateLimiter::default();
        let query = RecordingQueryPort::default();
        let response = controller(&limiter, &query).handle(12_345, 42, "hi");
        assert_eq!(query.calls.lock().unwrap().len(), 0);
        assert!(
            matches!(response, AskResponse::DeferredText { ephemeral: true, ref content } if content.to_ascii_lowercase().contains("more detailed"))
        );
    }

    #[test]
    fn test_too_long_question_rejected() {
        let limiter = RecordingRateLimiter::default();
        let query = RecordingQueryPort::default();
        let response = controller(&limiter, &query).handle(12_345, 42, &"x".repeat(501));
        assert_eq!(query.calls.lock().unwrap().len(), 0);
        assert!(
            matches!(response, AskResponse::DeferredText { ephemeral: true, ref content } if content.to_ascii_lowercase().contains("too long"))
        );
    }

    #[test]
    fn test_uses_configured_rate_limit() {
        let limiter = RecordingRateLimiter::default();
        let query = RecordingQueryPort::default();
        let _ = controller(&limiter, &query).handle(12_345, 42, "what is the average rating?");
        assert_eq!(
            *limiter.requests.lock().unwrap(),
            [RateLimitRequest {
                guild_id: 12_345,
                user_id: 42,
                limit: 3,
                per_seconds: 17,
            }]
        );
    }

    #[test]
    fn test_rate_limit_short_circuits() {
        let limiter = RecordingRateLimiter::default();
        *limiter.decision.lock().unwrap() = Some(RateLimitDecision {
            allowed: false,
            retry_after_seconds: 42.0,
        });
        let query = RecordingQueryPort::default();
        let response =
            controller(&limiter, &query).handle(12_345, 42, "what is the average rating?");
        assert_eq!(query.calls.lock().unwrap().len(), 0);
        assert!(
            matches!(response, AskResponse::ImmediateText { ephemeral: true, ref content } if content.contains("Rate limited") && content.contains("42"))
        );
    }

    #[test]
    fn test_success_renders_blue_embed_with_formatted_answer() {
        let limiter = RecordingRateLimiter::default();
        let query = RecordingQueryPort::default();
        *query.result.lock().unwrap() = QueryResult {
            success: true,
            results: vec![
                row([
                    ("discord_username", Value::Text("Alice".into())),
                    ("wins", Value::Integer(12)),
                ]),
                row([
                    ("discord_username", Value::Text("Bob".into())),
                    ("wins", Value::Integer(7)),
                ]),
            ],
            row_count: 2,
            ..QueryResult::default()
        };
        let response = controller(&limiter, &query).handle(12_345, 42, "who wins the most?");
        assert_eq!(
            *query.calls.lock().unwrap(),
            [(12_345, "who wins the most?".to_owned(), 42)]
        );
        let AskResponse::DeferredEmbed { embed } = response else {
            panic!("expected embed");
        };
        assert_eq!(embed.color, EmbedColor::Blue);
        assert_eq!(embed.fields[0].name, "Question");
        assert_eq!(embed.fields[1].name, "Answer");
        assert!(embed.fields[1].value.contains("Alice"));
        assert!(embed.fields[1].value.contains("Bob"));
    }

    #[test]
    fn test_question_truncated_to_256_in_embed() {
        let limiter = RecordingRateLimiter::default();
        let query = RecordingQueryPort::default();
        *query.result.lock().unwrap() = QueryResult {
            success: true,
            ..QueryResult::default()
        };
        let response = controller(&limiter, &query).handle(12_345, 42, &"Q".repeat(400));
        let AskResponse::DeferredEmbed { embed } = response else {
            panic!("expected embed");
        };
        assert_eq!(embed.fields[0].value, "Q".repeat(256));
    }

    #[test]
    fn test_failure_renders_red_embed_without_leaking_error() {
        let limiter = RecordingRateLimiter::default();
        let query = RecordingQueryPort::default();
        *query.result.lock().unwrap() = QueryResult {
            success: false,
            error: Some("ZZZINTERNALSENTINELZZZ near keyword".into()),
            ..QueryResult::default()
        };
        let response =
            controller(&limiter, &query).handle(12_345, 42, "what is the average rating?");
        let AskResponse::DeferredEmbed { embed } = response else {
            panic!("expected embed");
        };
        assert_eq!(embed.color, EmbedColor::Red);
        let text = embed
            .fields
            .iter()
            .map(|field| field.value.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("ZZZINTERNALSENTINELZZZ"));
        assert!(text.contains("I couldn't answer that"));
    }

    #[test]
    fn test_format_no_results() {
        assert_eq!(
            format_query_result(
                &QueryResult {
                    success: true,
                    ..QueryResult::default()
                },
                1_024
            ),
            "No results found."
        );
    }

    #[test]
    fn test_format_failure_returns_error() {
        assert!(
            format_query_result(
                &QueryResult {
                    success: false,
                    error: Some("boom".into()),
                    ..QueryResult::default()
                },
                1_024
            )
            .contains("boom")
        );
    }

    #[test]
    fn test_format_single_row_does_not_number() {
        let result = QueryResult {
            success: true,
            results: vec![row([
                ("discord_username", Value::Text("Solo".into())),
                ("wins", Value::Integer(5)),
            ])],
            row_count: 1,
            ..QueryResult::default()
        };
        let output = format_query_result(&result, 1_024);
        assert!(!output.starts_with("1."));
        assert!(output.contains("Solo"));
        assert!(output.contains('5'));
    }

    #[test]
    fn test_format_multi_row_numbers_each_line() {
        let result = QueryResult {
            success: true,
            results: vec![
                row([
                    ("discord_username", Value::Text("A".into())),
                    ("wins", Value::Integer(10)),
                ]),
                row([
                    ("discord_username", Value::Text("B".into())),
                    ("wins", Value::Integer(7)),
                ]),
            ],
            row_count: 2,
            ..QueryResult::default()
        };
        let output = format_query_result(&result, 1_024);
        assert!(output.starts_with("1."));
        assert!(output.contains("\n2."));
    }

    #[test]
    fn test_format_caps_at_ten_visible_rows() {
        let results = (0..12)
            .map(|index| {
                row([
                    ("discord_username", Value::Text(format!("P{index}"))),
                    ("wins", Value::Integer(index)),
                ])
            })
            .collect();
        let output = format_query_result(
            &QueryResult {
                success: true,
                results,
                row_count: 12,
                ..QueryResult::default()
            },
            1_024,
        );
        assert!(output.contains("1. "));
        assert!(output.contains("10. "));
        assert!(!output.contains("11. "));
        assert!(output.contains("and 2 more"));
    }

    #[test]
    fn stronger_unicode_truncation_counts_scalars_not_bytes() {
        let result = QueryResult {
            success: true,
            results: vec![BTreeMap::from([(
                "name".to_owned(),
                Value::Text("😀".repeat(20)),
            )])],
            row_count: 1,
            ..QueryResult::default()
        };
        let output = format_query_result(&result, 30);
        assert!(output.is_char_boundary(output.len()));
    }
}
