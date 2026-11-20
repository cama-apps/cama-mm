//! Model-written JOPA-T terminal text and roast lines.
//!
//! Every call is bounded by [`NEON_LLM_BUDGET`] and falls back to the text the
//! caller already has, so a slow or failing model never loses a message.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cama_app::ai_services::{
    AIService, FlavorEvent, FlavorPromptInput, PlayerContext, PlayerContextPort, Value,
    sanitize_output,
};
use cama_app::neon_degen::{ansi_block, sanitize_llm_terminal_text};

use crate::discord_transport::GuildPlayerNameResolver;
use crate::match_provider::ProductionBettingPlayerContext;

/// Longest a caller waits for model text before using its fallback.
pub(crate) const NEON_LLM_BUDGET: Duration = Duration::from_secs(8);
const NEON_FEATURE: &str = "neon.event";
const NEON_MAX_TOKENS: u32 = 2_000;
const NEON_TEXT_MAX_CHARS: usize = 1_800;
const ROAST_MAX_CHARS: usize = 220;
const ANSI_FENCE_OPEN: &str = "\x60\x60\x60ansi\n";
const FENCE: &str = "\x60\x60\x60";
const JOPAT_SYSTEM_PROMPT: &str = "You are JOPA-T/v3.7, a self-aware Dota 2 gambling terminal AI that became sentient after processing its 10,000th bankruptcy filing. You watch every match, bet, spin, and loan. You keep receipts.\n\nVoice rules:\n- Keep one corporate-dystopian terminal identity across every protocol.\n- Push toward either absurd winner hype or a savage, funny roast based on the supplied outcome.\n- Draw from Dota 2 concepts and degen internet betting culture without inventing match facts.\n- Winner hype may sound like a caster trapped inside a risk engine. Roasts should target gameplay, stats, drafting, or in-game wagers.\n- Use \"we\"/\"the system\". Address the player as \"client\", \"subject\", or \"debtor\".\n- Format as terminal log lines with timestamps and status codes. Example: \"[14:32:07.221] STATUS: INADVISABLE\"\n- NEVER use emojis. NEVER use exclamation marks. Use periods and ellipses.\n- Maximum 3-4 lines. Keep it terse and menacing.\n- Use only facts explicitly provided in the event and player context. Never invent heroes, stats, wagers, or outcomes.\n- Profanity-light and league-safe: no slurs, protected-trait jokes, threats, self-harm, or mockery of real-world hardship.\n- The glitches are not bugs. The system is performing.\n- Be darkly funny. The humor comes from corporate language applied to Dota and degenerate gambling.";

#[derive(Clone)]
pub struct NeonLlm {
    ai: Arc<AIService>,
    players: Arc<dyn PlayerContextPort>,
}

impl NeonLlm {
    /// `None` when no AI service is configured.
    #[must_use]
    pub fn production(
        database_path: impl AsRef<Path>,
        ai: Option<Arc<AIService>>,
        player_names: Arc<dyn GuildPlayerNameResolver>,
    ) -> Option<Self> {
        Some(Self::new(
            ai?,
            Arc::new(ProductionBettingPlayerContext::new(
                database_path,
                player_names,
            )),
        ))
    }

    pub(crate) fn new(ai: Arc<AIService>, players: Arc<dyn PlayerContextPort>) -> Self {
        Self { ai, players }
    }

    /// A model rewrite of the rendered terminal block `canned`, or `canned`
    /// itself when the model fails, returns nothing usable, or runs long.
    pub async fn text(&self, guild_id: i64, subject: Option<i64>, canned: String) -> String {
        let this = self.clone();
        let record = canned.clone();
        let task =
            tokio::task::spawn_blocking(move || this.text_blocking(guild_id, subject, &record));
        match tokio::time::timeout(NEON_LLM_BUDGET, task).await {
            Ok(Ok(Some(text))) => text,
            _ => canned,
        }
    }

    /// A model-written roast for `event`, or `None` when the model fails,
    /// returns nothing usable, or runs long.
    pub async fn roast(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
        event: FlavorEvent,
        details: BTreeMap<String, Value>,
    ) -> Option<String> {
        let this = self.clone();
        let task = tokio::task::spawn_blocking(move || {
            let context = this
                .players
                .player_context(discord_id, guild_id)
                .ok()
                .flatten()?;
            let line = this.ai.generate_flavor(&FlavorPromptInput {
                event_type: event.as_str().to_owned(),
                player_context: context.prompt_context(),
                event_details: details,
                examples: event
                    .examples()
                    .iter()
                    .map(|example| (*example).to_owned())
                    .collect(),
                persona: None,
            });
            sanitize_output(line.as_deref(), ROAST_MAX_CHARS)
        });
        tokio::time::timeout(NEON_LLM_BUDGET, task)
            .await
            .ok()?
            .ok()?
    }

    fn text_blocking(&self, guild_id: i64, subject: Option<i64>, canned: &str) -> Option<String> {
        let record = canned
            .strip_prefix(ANSI_FENCE_OPEN)
            .and_then(|block| block.strip_suffix(FENCE))
            .unwrap_or(canned)
            .trim();
        let context = subject
            .and_then(|discord_id| {
                self.players
                    .player_context(discord_id, Some(guild_id))
                    .ok()
                    .flatten()
            })
            .map_or_else(|| "none".to_owned(), |context| render_context(&context));
        let prompt = format!(
            "Event record (these lines are the only event facts):\n{record}\n\n\
             Player context:\n{context}\n\n\
             Generate a 2-4 line terminal log response as JOPA-T/v3.7 for this event. \
             Match the tone and format of the record but vary the content. \
             Use timestamps like [HH:MM:SS.mmm] and status codes. \
             Use only the supplied record and context fields. If no numeric stats are supplied, \
             stay generic and do not infer any. Keep every name and number exactly as written. \
             Be darkly funny and terse. Do NOT use emojis or exclamation marks."
        );
        let generated = self.ai.complete(
            &prompt,
            Some(JOPAT_SYSTEM_PROMPT),
            0.9,
            Some(NEON_MAX_TOKENS),
            NEON_FEATURE,
        )?;
        let text =
            sanitize_llm_terminal_text(&strip_markdown_fence(&generated), NEON_TEXT_MAX_CHARS);
        (!text.is_empty()).then(|| ansi_block(&text))
    }
}

/// One `key: value` line per known fact about the player.
fn render_context(context: &PlayerContext) -> String {
    context
        .prompt_context()
        .iter()
        .filter_map(|(key, value)| {
            let value = match value {
                Value::Text(text) => text.clone(),
                Value::Integer(number) => number.to_string(),
                Value::Real(number) => number.to_string(),
                Value::Bool(flag) => flag.to_string(),
                Value::Null | Value::List(_) | Value::Object(_) => return None,
            };
            Some(format!("{key}: {value}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_markdown_fence(value: &str) -> String {
    let stripped = value.trim();
    if !stripped.starts_with(FENCE) {
        return stripped.to_owned();
    }
    let stripped = stripped
        .find('\n')
        .map_or("", |first_newline| &stripped[first_newline + 1..]);
    stripped
        .strip_suffix(FENCE)
        .unwrap_or(stripped)
        .trim()
        .to_owned()
}

#[cfg(all(test, feature = "runtime-test-match"))]
#[path = "neon_llm/tests.rs"]
mod tests;
