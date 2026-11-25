//! Lobby pleas and post-game bait meant to get players queuing.
//!
//! Prewritten lines cover the occasional first-game Easter egg and any
//! plea or post-game bait for which model text is unavailable.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use cama_domain::formatting::escape_discord_text;

use crate::ai_services::{AIService, FlavorPersona, betting_personas, contains_link_or_markup};
use crate::duel_flavor::sanitize_detail;
use crate::flavor_lines::{
    BAIT_BET_LOST, BAIT_BLAME_LOSER, BAIT_CROWN_WINNER, BAIT_GENERIC, BAIT_STREAK, BAIT_UPSET,
    BAIT_VERSUS, PLEA_LINES, RIGGED_PLEA_LINES, fill, mix, pick_line,
};

const PLEA_FEATURE: &str = "queue_hype.plea";
const HYPE_FEATURE: &str = "queue_hype.post_match";
const PLEA_MAX_CHARS: usize = 240;
const HYPE_MAX_CHARS: usize = 400;
/// Number of recent prewritten pleas considered when choosing a fresh line.
const RECENT_PLEAS: usize = 40;
const TEMPERATURE: f64 = 0.9;
/// Recorded matches that get a post-game message, out of every ten.
const HYPE_MATCHES_IN_TEN: u64 = 7;
/// A winner below this pre-match win chance counts as an upset.
const UPSET_WIN_PROBABILITY: f64 = 0.45;

/// What a lobby plea may mention. Names are readable display names.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PleaFacts {
    pub lobby: String,
    pub players_in: usize,
    pub players_needed: usize,
    pub names: Vec<String>,
}

/// What a post-game message may mention. Names are readable display names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HypeFacts {
    pub winning_side: String,
    pub winners: Vec<String>,
    pub losers: Vec<String>,
    /// Pre-match win chance of the winning team, from 0 to 1.
    pub winner_win_probability: Option<f64>,
    pub standout: Option<String>,
    pub standout_rating_gain: Option<f64>,
    pub top_bet_win: Option<(String, i64)>,
    pub top_bet_loss: Option<(String, i64)>,
    pub streaks: Vec<(String, i64)>,
}

/// Whether a recorded match gets a post-game message. Stable per match, so
/// a recovery re-run reaches the same answer.
#[must_use]
pub fn should_post_hype(match_id: i64) -> bool {
    mix(match_id.unsigned_abs()) % 10 < HYPE_MATCHES_IN_TEN
}

pub struct QueueHype {
    ai: Option<Arc<AIService>>,
    /// The prewritten pleas posted most recently, oldest first.
    recent_pleas: Mutex<VecDeque<&'static str>>,
}

impl QueueHype {
    #[must_use]
    pub const fn new(ai: Option<Arc<AIService>>) -> Self {
        Self {
            ai,
            recent_pleas: Mutex::new(VecDeque::new()),
        }
    }

    /// A plea for the day's first lobby. Blocking; never empty.
    #[must_use]
    pub fn plea(&self, facts: &PleaFacts, seed: u64) -> String {
        if mix(seed.wrapping_add(1)).is_multiple_of(20) {
            return self.prewritten_plea(RIGGED_PLEA_LINES, facts, seed);
        }
        let (system, prompt) = plea_prompt(facts, seed);
        self.model_line(&system, &prompt, PLEA_FEATURE, &facts.names, PLEA_MAX_CHARS)
            .unwrap_or_else(|| self.prewritten_plea(PLEA_LINES, facts, seed))
    }

    /// Prefer a fresh line, or the least recently posted if the pool is exhausted.
    fn prewritten_plea(&self, pool: &[&'static str], facts: &PleaFacts, seed: u64) -> String {
        let start = (mix(seed) % pool.len() as u64) as usize;
        let template = {
            let mut recent = self
                .recent_pleas
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let choices = pool[start..].iter().chain(&pool[..start]).copied();
            let template = choices
                .clone()
                .find(|line| !recent.contains(line))
                .unwrap_or_else(|| {
                    choices
                        .min_by_key(|line| recent.iter().rposition(|past| past == line))
                        .expect("prewritten plea pools are nonempty")
                });
            if recent.len() >= RECENT_PLEAS {
                recent.pop_front();
            }
            recent.push_back(template);
            template
        };
        let lobby = escape_discord_text(&facts.lobby);
        let players_in = facts.players_in.to_string();
        let needed = facts.players_needed.to_string();
        fill(
            template,
            &[("lobby", &lobby), ("in", &players_in), ("needed", &needed)],
        )
    }

    /// A pot-stirring line about a finished match. Blocking; never empty.
    #[must_use]
    pub fn hype(&self, facts: &HypeFacts, seed: u64) -> String {
        let (system, prompt) = hype_prompt(facts, seed);
        self.model_line(
            &system,
            &prompt,
            HYPE_FEATURE,
            &hype_names(facts),
            HYPE_MAX_CHARS,
        )
        .unwrap_or_else(|| hype_fallback(facts, seed))
    }

    fn model_line(
        &self,
        system: &str,
        prompt: &str,
        feature: &str,
        names: &[String],
        max_chars: usize,
    ) -> Option<String> {
        let raw = self
            .ai
            .as_ref()?
            .complete(prompt, Some(system), TEMPERATURE, None, feature)?;
        usable_line(&raw, names, max_chars)
    }
}

fn persona(seed: u64) -> &'static FlavorPersona {
    let personas = betting_personas();
    &personas[(mix(seed) % personas.len() as u64) as usize]
}

fn prompt_names(names: &[String]) -> String {
    names
        .iter()
        .map(|name| sanitize_detail(name))
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

fn plea_prompt(facts: &PleaFacts, seed: u64) -> (String, String) {
    let persona = persona(seed);
    let system = format!(
        "{}\n\nRight now you are not taking bets. The first inhouse game of the day is almost \
         full and needs one or two more players. Write ONE short, funny, shamelessly desperate \
         plea begging lurkers to join the lobby, in character as {}.\n\nRULES:\n\
         - At most 2 sentences and 200 characters.\n\
         - Plain text only: no mentions, links, hashtags, markdown, quotation marks or emoji.\n\
         - Use only the names and numbers given. Never invent stats.\n\
         - Do not insult anyone by name.\n\
         - PG-13. No slurs.\n\
         - Put a space after every period.",
        persona.system_prompt, persona.name
    );
    let mut prompt = format!(
        "Lobby: {}\nPlayers in: {} of {}\nStill needed: {}\n",
        sanitize_detail(&facts.lobby),
        facts.players_in,
        facts.players_in + facts.players_needed,
        facts.players_needed
    );
    let names = prompt_names(&facts.names);
    if !names.is_empty() {
        prompt.push_str(&format!("Already in: {names}\n"));
    }
    prompt.push_str("Write the plea.");
    (system, prompt)
}

fn hype_names(facts: &HypeFacts) -> Vec<String> {
    facts
        .winners
        .iter()
        .chain(&facts.losers)
        .chain(&facts.standout)
        .chain(facts.top_bet_win.iter().map(|(name, _)| name))
        .chain(facts.top_bet_loss.iter().map(|(name, _)| name))
        .chain(facts.streaks.iter().map(|(name, _)| name))
        .cloned()
        .collect()
}

fn hype_prompt(facts: &HypeFacts, seed: u64) -> (String, String) {
    let persona = persona(seed);
    let mut angles = vec!["Stir up a grudge between the two sides."];
    if !facts.losers.is_empty() {
        angles.push("Pin the loss on one named loser.");
    }
    if !facts.winners.is_empty() {
        angles.push("Crown one named winner far too enthusiastically.");
    }
    if !facts.winners.is_empty() && !facts.losers.is_empty() {
        angles.push("Pit one named winner against one named loser.");
    }
    if facts.top_bet_loss.is_some() {
        angles.push("Mock the biggest lost bet.");
    }
    let angle = angles[(mix(seed.wrapping_add(1)) % angles.len() as u64) as usize];
    let system = format!(
        "{}\n\nRight now you are not taking bets. A Dota 2 inhouse match just ended and you are \
         stirring the pot. Write ONE short message of pure engagement bait, in character as {}: \
         provoke replies, grudges and a rematch using ONLY the facts provided.\n\nRULES:\n\
         - {angle}\n\
         - 1 to 3 sentences, at most 280 characters.\n\
         - You may call out any player by name, exactly as written. Tease the play, never \
         real-life traits.\n\
         - Use only numbers that appear in the facts. Never invent stats, heroes, items or \
         scores.\n\
         - Do not give instructions or mention commands.\n\
         - Plain text only: no mentions, links, hashtags, markdown or quotation marks, at most \
         one emoji.\n\
         - PG-13. No slurs.\n\
         - Put a space after every period.",
        persona.system_prompt, persona.name
    );
    let mut prompt = format!("Result: {} won.\n", sanitize_detail(&facts.winning_side));
    let winners = prompt_names(&facts.winners);
    if !winners.is_empty() {
        prompt.push_str(&format!("Winners: {winners}\n"));
    }
    let losers = prompt_names(&facts.losers);
    if !losers.is_empty() {
        prompt.push_str(&format!("Losers: {losers}\n"));
    }
    if let Some(chance) = facts.winner_win_probability {
        prompt.push_str(&format!(
            "Pre-match win chance of the winning team: {}%\n",
            percent(chance)
        ));
    }
    if let Some(standout) = &facts.standout {
        let gain = facts
            .standout_rating_gain
            .map_or_else(String::new, |gain| format!(" ({gain:+.0} rating)"));
        prompt.push_str(&format!(
            "Standout winner: {}{gain}\n",
            sanitize_detail(standout)
        ));
    }
    if let Some((name, payout)) = &facts.top_bet_win {
        prompt.push_str(&format!(
            "Biggest bet payout: {} ({payout} jopacoin)\n",
            sanitize_detail(name)
        ));
    }
    if let Some((name, amount)) = &facts.top_bet_loss {
        prompt.push_str(&format!(
            "Biggest bet lost: {} ({amount} jopacoin)\n",
            sanitize_detail(name)
        ));
    }
    for (name, streak) in &facts.streaks {
        prompt.push_str(&format!(
            "New personal-best win streak: {} ({streak} in a row)\n",
            sanitize_detail(name)
        ));
    }
    prompt.push_str("Write the message.");
    (system, prompt)
}

fn percent(probability: f64) -> i64 {
    (probability * 100.0).round() as i64
}

/// Clean model output, or `None` when it is empty or carries links or markup.
fn usable_line(raw: &str, names: &[String], max_chars: usize) -> Option<String> {
    let spaced = raw
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let line = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = line
        .trim_matches(['"', '\u{201c}', '\u{201d}'])
        .trim()
        .chars()
        .take(max_chars)
        .collect::<String>();
    // A nickname such as "Mr.Bean" must not make the line read as a link, so
    // names whose only offence is a dot are exempt. A name carrying a scheme
    // or link syntax is not, and a line repeating it is rejected.
    let mut names = names
        .iter()
        .map(|name| sanitize_detail(name))
        .filter(|name| !name.is_empty() && !contains_link_or_markup(&name.replace('.', " ")))
        .collect::<Vec<_>>();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let masked = names.iter().fold(line.clone(), |text, name| {
        text.replace(name.as_str(), "name")
    });
    if line.is_empty() || contains_link_or_markup(&masked) {
        return None;
    }
    Some(line.replace('@', "\u{ff20}"))
}

fn seeded<T>(items: &[T], seed: u64) -> Option<&T> {
    (!items.is_empty()).then(|| &items[(mix(seed) % items.len() as u64) as usize])
}

/// A prewritten bait pool and the slot values it can be filled with.
type BaitOption = (&'static [&'static str], Vec<(&'static str, String)>);

fn hype_fallback(facts: &HypeFacts, seed: u64) -> String {
    let side = escape_discord_text(&facts.winning_side);
    let winner = facts
        .standout
        .as_ref()
        .or_else(|| seeded(&facts.winners, seed.wrapping_add(2)))
        .map(|name| escape_discord_text(name));
    let loser = seeded(&facts.losers, seed.wrapping_add(3)).map(|name| escape_discord_text(name));

    let mut options: Vec<BaitOption> = vec![(BAIT_GENERIC, vec![("side", side.clone())])];
    if let Some(loser) = &loser {
        options.push((BAIT_BLAME_LOSER, vec![("loser", loser.clone())]));
    }
    if let Some(winner) = &winner {
        options.push((BAIT_CROWN_WINNER, vec![("winner", winner.clone())]));
    }
    if let (Some(winner), Some(loser)) = (&winner, &loser) {
        options.push((
            BAIT_VERSUS,
            vec![("winner", winner.clone()), ("loser", loser.clone())],
        ));
    }
    if let Some((name, amount)) = &facts.top_bet_loss {
        options.push((
            BAIT_BET_LOST,
            vec![
                ("bettor", escape_discord_text(name)),
                ("amount", amount.to_string()),
            ],
        ));
    }
    if let Some(chance) = facts
        .winner_win_probability
        .filter(|chance| *chance < UPSET_WIN_PROBABILITY)
    {
        options.push((
            BAIT_UPSET,
            vec![
                ("side", side.clone()),
                ("chance", percent(chance).to_string()),
            ],
        ));
    }
    if let Some((name, streak)) = seeded(&facts.streaks, seed.wrapping_add(4)) {
        options.push((
            BAIT_STREAK,
            vec![
                ("player", escape_discord_text(name)),
                ("streak", streak.to_string()),
            ],
        ));
    }
    let (pool, slots) = seeded(&options, seed).expect("generic bait is always available");
    let slots = slots
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect::<Vec<_>>();
    pick_line(pool, seed.wrapping_add(1), &slots).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_services::{
        AiProvider, ProviderConfig, ProviderCredential, ProviderError, ProviderErrorKind,
        ProviderLoader, ProviderRequest, ProviderResponse,
    };
    use std::sync::Mutex;
    use std::time::Duration;

    struct ScriptedProvider {
        reply: Result<String, ProviderErrorKind>,
        requests: Mutex<Vec<ProviderRequest>>,
    }

    impl AiProvider for ScriptedProvider {
        fn complete(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            self.requests.lock().expect("requests").push(request);
            match &self.reply {
                Ok(content) => Ok(ProviderResponse {
                    content: Some(content.clone()),
                    ..ProviderResponse::default()
                }),
                Err(kind) => Err(ProviderError::new(*kind, "scripted failure")),
            }
        }
    }

    struct Loader(Arc<ScriptedProvider>);

    impl ProviderLoader for Loader {
        fn load(&self) -> Result<Arc<dyn AiProvider>, ProviderError> {
            Ok(self.0.clone())
        }
    }

    fn hype_with(reply: Result<&str, ProviderErrorKind>) -> (QueueHype, Arc<ScriptedProvider>) {
        let provider = Arc::new(ScriptedProvider {
            reply: reply.map(str::to_owned),
            requests: Mutex::new(Vec::new()),
        });
        let config = ProviderConfig::new(
            "test/queue-hype",
            ProviderCredential::new("test-key").expect("credential"),
            Duration::from_secs(5),
            500,
        )
        .expect("config");
        let ai = Arc::new(AIService::new(config, Arc::new(Loader(provider.clone()))));
        (QueueHype::new(Some(ai)), provider)
    }

    fn plea_facts() -> PleaFacts {
        PleaFacts {
            lobby: "All You Can Feed".to_owned(),
            players_in: 8,
            players_needed: 2,
            names: vec!["Alice".to_owned(), "Mr.Bean".to_owned()],
        }
    }

    fn hype_facts() -> HypeFacts {
        HypeFacts {
            winning_side: "Radiant".to_owned(),
            winners: vec!["Alice".to_owned(), "Bob".to_owned()],
            losers: vec!["Carol".to_owned(), "Mr.Bean".to_owned()],
            winner_win_probability: Some(0.38),
            standout: Some("Alice".to_owned()),
            standout_rating_gain: Some(18.4),
            top_bet_win: Some(("Dave".to_owned(), 900)),
            top_bet_loss: Some(("Erin".to_owned(), 250)),
            streaks: vec![("Bob".to_owned(), 6)],
        }
    }

    fn all_bait() -> Vec<&'static str> {
        [
            BAIT_GENERIC,
            BAIT_BLAME_LOSER,
            BAIT_CROWN_WINNER,
            BAIT_VERSUS,
            BAIT_BET_LOST,
            BAIT_UPSET,
            BAIT_STREAK,
        ]
        .concat()
    }

    #[test]
    fn plea_without_a_model_rotates_the_prewritten_pool() {
        let hype = QueueHype::new(None);
        let facts = plea_facts();
        let lines = (0..400)
            .map(|seed| hype.plea(&facts, seed))
            .collect::<std::collections::BTreeSet<_>>();
        assert!(lines.len() > 80, "pleas should rotate widely");
        for line in &lines {
            assert!(!line.is_empty() && !line.contains('{'), "{line}");
        }
    }

    #[test]
    fn rigging_pleas_are_rare_and_run_before_the_model() {
        let (online, provider) = hype_with(Ok("Join already."));
        let offline = QueueHype::new(None);
        let facts = plea_facts();
        let rigging_lines = RIGGED_PLEA_LINES
            .iter()
            .map(|line| fill(line, &[("needed", "2")]))
            .collect::<std::collections::BTreeSet<_>>();
        let first = online.plea(&facts, 28);
        assert!(rigging_lines.contains(&first));
        assert_eq!(first, offline.plea(&facts, 28));
        assert!(provider.requests.lock().expect("requests").is_empty());
        assert_eq!(online.plea(&facts, 1), "Join already.");

        let mut seen = std::collections::BTreeSet::new();
        let mut count = 0;
        for seed in 0..4_000 {
            let line = offline.plea(&facts, seed);
            if rigging_lines.contains(&line) {
                seen.insert(line);
                count += 1;
            }
        }
        assert_eq!(seen, rigging_lines);
        assert!((180..=220).contains(&count), "{count} rigging pleas");
    }

    #[test]
    fn a_prewritten_plea_is_not_repeated_while_it_is_recent() {
        let hype = QueueHype::new(None);
        let mut facts = plea_facts();
        let first = hype.plea(&facts, 9);
        assert_eq!(first, QueueHype::new(None).plea(&facts, 9));

        // The same lobby begging again, even with the same seed.
        let mut seen = std::collections::BTreeSet::from([first]);
        for _ in 1..RECENT_PLEAS {
            assert!(seen.insert(hype.plea(&facts, 9)), "a recent plea repeated");
        }

        // A changed head count does not make a recent line new again.
        let villain = (0..1_000)
            .find(|seed| {
                QueueHype::new(None)
                    .plea(&facts, *seed)
                    .contains("villain arcs")
            })
            .expect("seed for the villain line");
        let hype = QueueHype::new(None);
        assert!(hype.plea(&facts, villain).contains("villain arcs"));
        facts.players_in = 9;
        facts.players_needed = 1;
        assert!(!hype.plea(&facts, villain).contains("villain arcs"));
    }

    #[test]
    fn hype_without_a_model_uses_every_fact_group_and_readable_names() {
        let hype = QueueHype::new(None);
        let facts = hype_facts();
        let lines = (0..600)
            .map(|seed| hype.hype(&facts, seed))
            .collect::<Vec<_>>();
        for line in &lines {
            assert!(!line.is_empty() && !line.contains('{'), "{line}");
            assert!(!line.contains("<@") && !line.contains('/'), "{line}");
        }
        for needle in ["Erin", "250", "38%", "Bob has won 6", "Carol", "Alice"] {
            assert!(
                lines.iter().any(|line| line.contains(needle)),
                "no prewritten line used {needle}"
            );
        }
    }

    #[test]
    fn hype_without_player_facts_stays_generic() {
        let hype = QueueHype::new(None);
        let facts = HypeFacts {
            winning_side: "Dire".to_owned(),
            ..HypeFacts::default()
        };
        for seed in 0..200 {
            let line = hype.hype(&facts, seed);
            assert!(
                BAIT_GENERIC
                    .iter()
                    .any(|template| template.replace("{side}", "Dire") == line),
                "{line}"
            );
        }
    }

    #[test]
    fn an_upset_line_needs_an_underdog_winner() {
        let hype = QueueHype::new(None);
        let facts = HypeFacts {
            winning_side: "Dire".to_owned(),
            winner_win_probability: Some(0.61),
            ..HypeFacts::default()
        };
        assert!((0..300).all(|seed| !hype.hype(&facts, seed).contains('%')));
    }

    #[test]
    fn valid_model_output_is_used_and_unquoted() {
        let (hype, provider) = hype_with(Ok("\"Mr.Bean  fed again.\nRematch.\""));
        assert_eq!(hype.hype(&hype_facts(), 4), "Mr.Bean fed again. Rematch.");
        let requests = provider.requests.lock().expect("requests");
        assert_eq!(requests.len(), 1);
        let system = &requests[0].messages[0].content;
        let prompt = &requests[0].messages[1].content;
        assert!(
            betting_personas()
                .iter()
                .any(|persona| system.starts_with(&persona.system_prompt))
        );
        assert!(system.contains("Do not give instructions or mention commands."));
        for fact in [
            "Result: Radiant won.",
            "Winners: Alice, Bob",
            "Losers: Carol, Mr.Bean",
            "winning team: 38%",
            "Standout winner: Alice (+18 rating)",
            "Biggest bet payout: Dave (900 jopacoin)",
            "Biggest bet lost: Erin (250 jopacoin)",
            "win streak: Bob (6 in a row)",
        ] {
            assert!(prompt.contains(fact), "prompt is missing {fact}: {prompt}");
        }
    }

    #[test]
    fn prompts_omit_absent_facts_and_strip_markup_from_names() {
        let (_, prompt) = hype_prompt(
            &HypeFacts {
                winning_side: "Dire".to_owned(),
                winners: vec!["<@123> `x`".to_owned()],
                ..HypeFacts::default()
            },
            1,
        );
        assert_eq!(
            prompt,
            "Result: Dire won.\nWinners: 123 x\nWrite the message."
        );
        let (system, prompt) = plea_prompt(&plea_facts(), 1);
        assert!(system.contains("begging lurkers to join"));
        assert_eq!(
            prompt,
            "Lobby: All You Can Feed\nPlayers in: 8 of 10\nStill needed: 2\n\
             Already in: Alice, Mr.Bean\nWrite the plea."
        );
    }

    #[test]
    fn unusable_model_output_falls_back_to_a_prewritten_line() {
        for reply in [
            Err(ProviderErrorKind::Unavailable),
            Ok("<@123> gg"),
            Ok("see https://example.com"),
            Ok("visit example.com now"),
            Ok("   "),
        ] {
            let (hype, _) = hype_with(reply);
            let line = hype.hype(&hype_facts(), 2);
            assert_eq!(line, QueueHype::new(None).hype(&hype_facts(), 2));
            let plea = hype.plea(&plea_facts(), 2);
            assert_eq!(plea, QueueHype::new(None).plea(&plea_facts(), 2));
        }
        assert!(
            all_bait()
                .iter()
                .all(|line| !line.contains("<@") && !line.contains('/'))
        );
    }

    #[test]
    fn a_link_shaped_nickname_is_not_exempt_from_the_link_filter() {
        for nickname in [
            "https://evil.example/x",
            "javascript:alert",
            "[x](evil.example)",
        ] {
            let names = vec![nickname.to_owned(), "Mr.Bean".to_owned()];
            assert_eq!(
                usable_line(&format!("{nickname} fed again."), &names, 400),
                None,
                "{nickname}"
            );
        }
        let names = vec!["Mr.Bean".to_owned()];
        assert_eq!(
            usable_line("Mr.Bean fed. See evil.example now.", &names, 400),
            None
        );
        assert_eq!(
            usable_line("Mr.Bean fed again.", &names, 400).as_deref(),
            Some("Mr.Bean fed again.")
        );
    }

    #[test]
    fn model_output_cannot_ping() {
        let (hype, _) = hype_with(Ok("@everyone Alice threw"));
        assert_eq!(hype.hype(&hype_facts(), 2), "\u{ff20}everyone Alice threw");
    }

    #[test]
    fn plea_uses_valid_model_output() {
        let (hype, provider) = hype_with(Ok("Join already."));
        assert_eq!(hype.plea(&plea_facts(), 1), "Join already.");
        let requests = provider.requests.lock().expect("requests");
        assert_eq!(requests[0].temperature, Some(TEMPERATURE));
        assert!(requests[0].messages[1].content.contains("Still needed: 2"));
    }

    #[test]
    fn the_post_game_roll_is_stable_and_passes_about_seven_in_ten() {
        let posted = (1..=10_000).filter(|id| should_post_hype(*id)).count();
        assert!((6_700..=7_300).contains(&posted), "{posted}");
        assert_eq!(should_post_hype(42), should_post_hype(42));
    }
}
