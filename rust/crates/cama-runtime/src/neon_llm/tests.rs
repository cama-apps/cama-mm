use std::collections::BTreeMap;

use cama_app::ai_services::{PlayerContext, PlayerRecord};

use super::*;
use crate::discord_transport::DiscordIdPlayerNameResolver;
use crate::test_support::ScriptedAi;

const GUILD: i64 = 42;
const PLAYER: i64 = 7;

struct FixedPlayers(Option<PlayerContext>);

impl PlayerContextPort for FixedPlayers {
    fn player_context(
        &self,
        _discord_id: i64,
        _guild_id: Option<i64>,
    ) -> Result<Option<PlayerContext>, String> {
        Ok(self.0.clone())
    }
}

fn alice() -> PlayerContext {
    PlayerContext {
        bankruptcy_count: 3,
        total_loans: 12,
        ..PlayerContext::from_record(PlayerRecord {
            username: "Alice".to_owned(),
            balance: -40,
            wins: 6,
            losses: 4,
            lowest_balance: Some(-500),
        })
    }
}

fn llm(ai: &Arc<ScriptedAi>, player: Option<PlayerContext>) -> NeonLlm {
    NeonLlm::new(ai.service(), Arc::new(FixedPlayers(player)))
}

fn canned() -> String {
    ansi_block("[LEDGER] Client: Alice\nBalance: -40 JC")
}

fn config(values: &[(&str, &str)]) -> ApplicationConfig {
    let values = values
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect::<BTreeMap<_, _>>();
    ApplicationConfig::from_lookup(|key| values.get(key).cloned()).expect("valid config")
}

#[tokio::test]
async fn neon_llm_needs_an_ai_service_and_the_switch() {
    let ai = ScriptedAi::replying("ok");
    let on = config(&[("DISCORD_BOT_TOKEN", "token")]);
    let off = config(&[("DISCORD_BOT_TOKEN", "token"), ("FLAVOR_LLM_ENABLED", "no")]);
    let production = |config: &ApplicationConfig, ai: Option<Arc<AIService>>| {
        NeonLlm::production(
            "unused.db",
            config,
            ai,
            Arc::new(DiscordIdPlayerNameResolver),
        )
    };

    assert!(production(&on, Some(ai.service())).is_some());
    assert!(production(&on, None).is_none());
    assert!(production(&off, Some(ai.service())).is_none());
}

#[tokio::test]
async fn model_text_replaces_the_template_inside_an_ansi_block() {
    let ai = ScriptedAi::replying(
        "\x60\x60\x60\n[14:02:11.004] STATUS: INSOLVENT\n[14:02:11.005] Client Alice owes 40 JC.\n\x60\x60\x60",
    );

    let text = llm(&ai, Some(alice()))
        .text(GUILD, Some(PLAYER), canned())
        .await;

    assert_eq!(
        text,
        ansi_block("[14:02:11.004] STATUS: INSOLVENT\n[14:02:11.005] Client Alice owes 40 JC.")
    );
    let requests = ai.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].messages[0]
            .content
            .starts_with("You are JOPA-T/v3.7")
    );
    let prompt = &requests[0].messages[1].content;
    assert!(
        prompt.starts_with(
            "Event record (these lines are the only event facts):\n\
             [LEDGER] Client: Alice\nBalance: -40 JC\n\nPlayer context:\n"
        ),
        "{prompt}"
    );
    for fact in [
        "username: Alice",
        "balance: -40",
        "bankruptcy_count: 3",
        "total_loans: 12",
        "win_rate: 60%",
        "lowest_balance: -500",
    ] {
        assert!(prompt.contains(fact), "prompt is missing {fact}: {prompt}");
    }
    assert!(!prompt.contains('\x60'), "the record is passed unfenced");
    assert!(!prompt.contains("degen_score"), "absent facts are left out");
    assert_eq!(requests[0].max_tokens, NEON_MAX_TOKENS);
}

#[tokio::test]
async fn a_subject_without_a_player_record_gets_no_context() {
    let ai = ScriptedAi::replying("[00:00:00.001] STATUS: NOTED");

    let server_wide = llm(&ai, Some(alice())).text(GUILD, None, canned()).await;
    let unknown = llm(&ai, None).text(GUILD, Some(PLAYER), canned()).await;

    assert_eq!(server_wide, ansi_block("[00:00:00.001] STATUS: NOTED"));
    assert_eq!(unknown, server_wide);
    for request in ai.requests() {
        assert!(
            request.messages[1]
                .content
                .contains("Player context:\nnone\n")
        );
    }
}

#[tokio::test]
async fn failed_or_unusable_model_output_keeps_the_template() {
    for ai in [
        ScriptedAi::failing(),
        ScriptedAi::replying("   "),
        ScriptedAi::replying("\x60\x60\x60ansi\n\x60\x60\x60"),
        ScriptedAi::replying("\u{1b}[31m\u{1b}[0m"),
    ] {
        let text = llm(&ai, Some(alice()))
            .text(GUILD, Some(PLAYER), canned())
            .await;
        assert_eq!(text, canned());
        assert_eq!(ai.requests().len(), 1);
    }
}

#[tokio::test]
async fn model_output_cannot_break_out_of_the_code_block_or_ping() {
    let ai = ScriptedAi::replying(
        "[1] done \x60\x60\x60 escaped\n[2] @everyone <@123> pay up\n[3] c\n[4] d\n[5] e",
    );

    let text = llm(&ai, None).text(GUILD, None, canned()).await;

    let body = text
        .strip_prefix("\x60\x60\x60ansi\n")
        .and_then(|text| text.strip_suffix("\n\x60\x60\x60"))
        .expect("an ANSI block");
    assert!(!body.contains('\x60'), "{body}");
    assert!(
        !body.contains("@everyone") && !body.contains("<@123>"),
        "{body}"
    );
    assert_eq!(body.lines().count(), 4, "at most four terminal lines");
}

#[tokio::test(start_paused = true)]
async fn a_slow_model_falls_back_after_the_budget() {
    let ai = ScriptedAi::held("[00:00:00.001] STATUS: LATE");
    let llm = llm(&ai, None);

    let pending = tokio::spawn(async move { llm.text(GUILD, None, canned()).await });
    while ai.requests().is_empty() {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(NEON_LLM_BUDGET).await;

    assert_eq!(pending.await.expect("text task"), canned());
    ai.release();
}

#[tokio::test]
async fn roast_is_a_model_line_built_from_player_history() {
    let ai = ScriptedAi::replying("Twelve loans and still asking.");

    let line = llm(&ai, Some(alice()))
        .roast(
            Some(GUILD),
            PLAYER,
            FlavorEvent::LoanTaken,
            BTreeMap::from([("amount".to_owned(), Value::Integer(100))]),
        )
        .await;

    assert_eq!(line.as_deref(), Some("Twelve loans and still asking."));
    let requests = ai.requests();
    let prompt = &requests[0].messages[1].content;
    assert!(
        prompt.starts_with("Event: loan_taken\nEvent details: amount=100\nPlayer: Alice\n"),
        "{prompt}"
    );
    assert!(prompt.contains("total_loans=12") && prompt.contains("bankruptcy_count=3"));
    assert!(
        requests[0].messages[0]
            .content
            .contains("Your financial advisor just felt a disturbance in the force.")
    );
}

#[tokio::test]
async fn roast_is_none_when_nothing_usable_comes_back() {
    let details = BTreeMap::new;
    for ai in [
        ScriptedAi::failing(),
        ScriptedAi::replying("see https://example.com"),
        ScriptedAi::replying("<@123> broke again"),
    ] {
        let line = llm(&ai, Some(alice()))
            .roast(
                Some(GUILD),
                PLAYER,
                FlavorEvent::BankruptcyDeclared,
                details(),
            )
            .await;
        assert_eq!(line, None);
    }
    let ai = ScriptedAi::replying("Broke again.");
    let line = llm(&ai, None)
        .roast(
            Some(GUILD),
            PLAYER,
            FlavorEvent::BankruptcyDeclared,
            details(),
        )
        .await;
    assert_eq!(line, None, "an unregistered player has no history to roast");
    assert!(ai.requests().is_empty());
}
