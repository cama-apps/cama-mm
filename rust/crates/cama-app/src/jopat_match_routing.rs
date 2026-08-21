//! Discord-neutral routing for JOPA-T match, pet, and enrichment callouts.
//!
//! Python remains the live Discord, configuration, AI-provider, GIF-renderer,
//! player-name, hero-catalog, and random-number authority during migration.
//! This module owns the tested ordering and selection policy. Every live edge
//! is represented by a typed port so the Rust policy can run without a gateway
//! or a second persistence authority.

use std::collections::{BTreeMap, VecDeque};

use cama_domain::pet::get_species;

use crate::dig_neon::BigWinFlavor;
use crate::jopat_post_match::{
    JopatChoicePort, JopatPostMatchContext, NumericFact, PromptFact, build_event_description,
    choose_protocol, render_fallback,
};
use crate::neon_degen::{
    AiRequest, ContextValue, GenerationOptions, GifAsset, NeonResult, PlayerContext, generate_text,
};
use crate::post_match_gif_media::render_post_match_gif;

pub const POST_MATCH_BASE_CHANCE: f64 = 0.35;
pub const POST_MATCH_NOTABLE_CHANCE: f64 = 0.55;
pub const POST_MATCH_EXTREME_CHANCE: f64 = 0.75;
pub const POST_MATCH_GIF_CHANCE: f64 = 0.20;
pub const DEFAULT_MVP_CHANCE: f64 = 0.35;
pub const BIG_WIN_MIN_PAYOUT: i64 = 500;
pub const DEGEN_MILESTONE_SCORE: i64 = 90;
pub const PET_ANNOUNCEMENT_LIMIT: usize = 10;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePortError(pub String);

pub trait JopatMatchRandomPort: JopatChoicePort {
    fn roll(&mut self, chance: f64) -> bool;
}

#[derive(Clone, Debug)]
pub struct SeededJopatMatchRandom {
    state: u64,
}

impl SeededJopatMatchRandom {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        let mut state = self.state.max(1);
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.state = state;
        state
    }
}

impl JopatChoicePort for SeededJopatMatchRandom {
    fn choose_index(&mut self, option_count: usize) -> usize {
        if option_count == 0 {
            return 0;
        }
        usize::try_from(self.next() % option_count as u64).unwrap_or(0)
    }
}

impl JopatMatchRandomPort for SeededJopatMatchRandom {
    fn roll(&mut self, chance: f64) -> bool {
        self.next() as f64 / (u64::MAX as f64) < chance.clamp(0.0, 1.0)
    }
}

#[derive(Clone, Debug)]
pub struct ScriptedJopatMatchRandom {
    rolls: VecDeque<bool>,
    default_roll: bool,
    choices: VecDeque<usize>,
    observed_chances: Vec<f64>,
}

impl ScriptedJopatMatchRandom {
    #[must_use]
    pub fn always(value: bool) -> Self {
        Self {
            rolls: VecDeque::new(),
            default_roll: value,
            choices: VecDeque::new(),
            observed_chances: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_rolls(rolls: impl IntoIterator<Item = bool>) -> Self {
        Self {
            rolls: rolls.into_iter().collect(),
            default_roll: false,
            choices: VecDeque::new(),
            observed_chances: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_choices(mut self, choices: impl IntoIterator<Item = usize>) -> Self {
        self.choices = choices.into_iter().collect();
        self
    }

    #[must_use]
    pub fn observed_chances(&self) -> &[f64] {
        &self.observed_chances
    }
}

impl JopatChoicePort for ScriptedJopatMatchRandom {
    fn choose_index(&mut self, option_count: usize) -> usize {
        if option_count == 0 {
            return 0;
        }
        self.choices.pop_front().unwrap_or(0) % option_count
    }
}

impl JopatMatchRandomPort for ScriptedJopatMatchRandom {
    fn roll(&mut self, chance: f64) -> bool {
        self.observed_chances.push(chance);
        self.rolls.pop_front().unwrap_or(self.default_roll)
    }
}

pub trait JopatAiPort {
    fn complete(&mut self, request: &AiRequest) -> Result<Option<String>, RuntimePortError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopJopatAi;

impl JopatAiPort for NoopJopatAi {
    fn complete(&mut self, _request: &AiRequest) -> Result<Option<String>, RuntimePortError> {
        Ok(None)
    }
}

pub trait JopatIdentityPort {
    fn player_name(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
    ) -> Result<Option<String>, RuntimePortError>;

    fn hero_name(&mut self, hero_id: u16) -> Result<Option<String>, RuntimePortError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FallbackJopatIdentity;

impl JopatIdentityPort for FallbackJopatIdentity {
    fn player_name(
        &mut self,
        _discord_id: u64,
        _guild_id: Option<u64>,
    ) -> Result<Option<String>, RuntimePortError> {
        Ok(None)
    }

    fn hero_name(&mut self, _hero_id: u16) -> Result<Option<String>, RuntimePortError> {
        Ok(None)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PostMatchGifKind {
    BuybackDenied,
    OddsAnomaly,
    BeyondGodlike,
    DivineRapierPosition,
    AncientLiquidated,
}

impl PostMatchGifKind {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::BuybackDenied => "buyback_denied",
            Self::OddsAnomaly => "odds_anomaly",
            Self::BeyondGodlike => "beyond_godlike",
            Self::DivineRapierPosition => "divine_rapier_position",
            Self::AncientLiquidated => "ancient_liquidated",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PostMatchGifSpec {
    pub kind: PostMatchGifKind,
    pub client_name: String,
    pub value: i64,
}

pub trait PostMatchGifPort {
    fn render(&mut self, spec: &PostMatchGifSpec) -> Result<GifAsset, RuntimePortError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ContractGifRenderer;

impl PostMatchGifPort for ContractGifRenderer {
    fn render(&mut self, spec: &PostMatchGifSpec) -> Result<GifAsset, RuntimePortError> {
        render_post_match_gif(&spec.client_name, spec.value, spec.kind.key())
            .map_err(|error| RuntimePortError(error.to_string()))
    }
}

#[must_use]
pub fn post_match_chance(context: &JopatPostMatchContext) -> f64 {
    let rating_change = numeric(context.rating_change.as_ref());
    let expected_win_prob = numeric(context.expected_win_prob.as_ref());
    let degen_score = numeric(context.degen_score.as_ref());
    let extreme = context.payout >= 750
        || context.loss >= 500
        || context.leverage >= 5
        || context.bankruptcy_count >= 2
        || degen_score.is_some_and(|score| score >= 95.0)
        || rating_change.is_some_and(|change| change.abs() >= 40.0)
        || expected_win_prob.is_some_and(|probability| probability <= 0.30)
        || context.streak.abs() >= 7;
    if extreme {
        return POST_MATCH_EXTREME_CHANCE;
    }
    let notable = context.payout >= BIG_WIN_MIN_PAYOUT
        || context.loss >= 200
        || context.leverage >= 3
        || context.bankruptcy_count > 0
        || degen_score.is_some_and(|score| score >= 80.0)
        || rating_change.is_some_and(|change| change.abs() >= 20.0)
        || expected_win_prob.is_some_and(|probability| probability < 0.45)
        || context.streak.abs() >= 4;
    if notable {
        POST_MATCH_NOTABLE_CHANCE
    } else {
        POST_MATCH_BASE_CHANCE
    }
}

#[must_use]
pub fn post_match_gif_theme(context: &JopatPostMatchContext) -> Option<PostMatchGifSpec> {
    if context.loss >= 500 && context.leverage >= 5 {
        return Some(PostMatchGifSpec {
            kind: PostMatchGifKind::BuybackDenied,
            client_name: context
                .loser_name
                .clone()
                .unwrap_or_else(|| "UNKNOWN CLIENT".to_owned()),
            value: context.loss,
        });
    }
    if let (Some(winner_name), Some(expected_win_prob)) = (
        context.winner_name.as_ref(),
        numeric(context.expected_win_prob.as_ref()),
    ) && expected_win_prob <= 0.30
    {
        return Some(PostMatchGifSpec {
            kind: PostMatchGifKind::OddsAnomaly,
            client_name: winner_name.clone(),
            value: (expected_win_prob * 100.0).round() as i64,
        });
    }
    if context.streak >= 7 {
        return Some(PostMatchGifSpec {
            kind: PostMatchGifKind::BeyondGodlike,
            client_name: context
                .winner_name
                .clone()
                .unwrap_or_else(|| "UNKNOWN CLIENT".to_owned()),
            value: context.streak,
        });
    }
    if let Some(rating_change) = numeric(context.rating_change.as_ref())
        && rating_change >= 40.0
    {
        return Some(PostMatchGifSpec {
            kind: PostMatchGifKind::DivineRapierPosition,
            client_name: context
                .winner_name
                .clone()
                .unwrap_or_else(|| "UNKNOWN CLIENT".to_owned()),
            value: rating_change.round() as i64,
        });
    }
    (context.payout >= 750).then(|| PostMatchGifSpec {
        kind: PostMatchGifKind::AncientLiquidated,
        client_name: context
            .winner_name
            .clone()
            .unwrap_or_else(|| "UNKNOWN CLIENT".to_owned()),
        value: context.payout,
    })
}

fn numeric(value: Option<&NumericFact>) -> Option<f64> {
    value?.as_str().parse().ok()
}

pub struct JopatMatchService<R, I, A, G> {
    enabled: bool,
    ai_enabled: bool,
    mvp_chance: f64,
    random: R,
    identity: I,
    ai: A,
    gifs: G,
}

impl<R, I, A, G> JopatMatchService<R, I, A, G>
where
    R: JopatMatchRandomPort,
    I: JopatIdentityPort,
    A: JopatAiPort,
    G: PostMatchGifPort,
{
    #[must_use]
    pub const fn new(random: R, identity: I, ai: A, gifs: G) -> Self {
        Self {
            enabled: true,
            ai_enabled: false,
            mvp_chance: DEFAULT_MVP_CHANCE,
            random,
            identity,
            ai,
            gifs,
        }
    }

    #[must_use]
    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    #[must_use]
    pub const fn with_ai_enabled(mut self, enabled: bool) -> Self {
        self.ai_enabled = enabled;
        self
    }

    #[must_use]
    pub const fn with_mvp_chance(mut self, chance: f64) -> Self {
        self.mvp_chance = chance;
        self
    }

    #[must_use]
    pub const fn random(&self) -> &R {
        &self.random
    }

    #[must_use]
    pub const fn identity(&self) -> &I {
        &self.identity
    }

    #[must_use]
    pub const fn ai(&self) -> &A {
        &self.ai
    }

    pub fn on_post_match_debrief(
        &mut self,
        guild_id: Option<u64>,
        mut context: JopatPostMatchContext,
        winner_id: Option<u64>,
        loser_id: Option<u64>,
    ) -> Option<NeonResult> {
        if !self.enabled {
            return None;
        }
        if context.winner_name.is_none()
            && let Some(discord_id) = winner_id
        {
            context.winner_name = Some(self.resolve_player_name(discord_id, guild_id));
        }
        if context.loser_name.is_none()
            && let Some(discord_id) = loser_id
        {
            context.loser_name = Some(self.resolve_player_name(discord_id, guild_id));
        }
        if !self.random.roll(post_match_chance(&context)) {
            return None;
        }
        let text = self.render_context_text(&context);
        if let Some(spec) = post_match_gif_theme(&context)
            && self.random.roll(POST_MATCH_GIF_CHANCE)
            && let Ok(gif) = self.gifs.render(&spec)
        {
            return Some(NeonResult {
                layer: 3,
                text_block: Some(text),
                gif_file: Some(gif),
                footer_text: None,
            });
        }
        Some(NeonResult::text(2, text))
    }

    #[must_use]
    pub fn on_match_enriched(
        &mut self,
        guild_id: Option<u64>,
        winners: &[EnrichedPlayer],
        losers: &[EnrichedPlayer],
    ) -> Vec<NeonResult> {
        if !self.enabled {
            return Vec::new();
        }
        let enriched_winners = winners
            .iter()
            .filter(|player| player.has_telemetry())
            .collect::<Vec<_>>();
        let extreme_losers = losers
            .iter()
            .filter(|player| player.is_extreme_loser())
            .collect::<Vec<_>>();
        if enriched_winners.is_empty() && extreme_losers.is_empty() {
            return Vec::new();
        }
        if !self.random.roll(self.mvp_chance) {
            return Vec::new();
        }
        let (target, is_winner) = if extreme_losers.is_empty() {
            let target = enriched_winners
                .into_iter()
                .max_by_key(|player| player.winner_selection_key())
                .expect("non-empty enriched winners");
            (target, true)
        } else {
            let target = extreme_losers
                .into_iter()
                .max_by_key(|player| player.loser_selection_key())
                .expect("non-empty extreme losers");
            (target, false)
        };
        let player_name = target.discord_id.map_or_else(
            || "Unknown client".to_owned(),
            |discord_id| self.resolve_player_name(discord_id, guild_id),
        );
        let hero = target
            .hero_id
            .and_then(|hero_id| self.identity.hero_name(hero_id).ok().flatten());
        let context = JopatPostMatchContext {
            winner_name: is_winner.then_some(player_name.clone()),
            loser_name: (!is_winner).then_some(player_name),
            hero,
            kills: target.kills.map(i64::from).map(NumericFact::from),
            deaths: target.deaths.map(i64::from).map(NumericFact::from),
            assists: target.assists.map(i64::from).map(NumericFact::from),
            gpm: target.gpm.map(i64::from).map(NumericFact::from),
            xpm: target.xpm.map(i64::from).map(NumericFact::from),
            ..JopatPostMatchContext::default()
        };
        let text = self.render_context_text(&context);
        let mention = target
            .discord_id
            .map_or_else(String::new, |discord_id| format!("<@{discord_id}>\n"));
        vec![NeonResult::text(2, format!("{mention}{text}"))]
    }

    fn resolve_player_name(&mut self, discord_id: u64, guild_id: Option<u64>) -> String {
        self.identity
            .player_name(discord_id, guild_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| format!("Client-{}", discord_id % 10_000))
    }

    fn render_context_text(&mut self, context: &JopatPostMatchContext) -> String {
        let protocol = choose_protocol(context, &mut self.random);
        let fallback = render_fallback(protocol, context, &mut self.random);
        let event_description = build_event_description(protocol, context);
        let player_context = to_player_context(context);
        if !self.ai_enabled {
            return fallback;
        }
        let preview = generate_text(
            true,
            &event_description,
            &player_context,
            &fallback,
            None,
            GenerationOptions {
                anonymous: false,
                validate_facts: true,
            },
        );
        let Some(request) = preview.request else {
            return fallback;
        };
        let Ok(Some(output)) = self.ai.complete(&request) else {
            return fallback;
        };
        generate_text(
            true,
            &event_description,
            &player_context,
            &fallback,
            Some(&output),
            GenerationOptions {
                anonymous: false,
                validate_facts: true,
            },
        )
        .text
    }
}

fn to_player_context(context: &JopatPostMatchContext) -> PlayerContext {
    context
        .prompt_context()
        .into_iter()
        .map(|(key, value)| {
            let value = match value {
                PromptFact::Text(value) => ContextValue::Text(value),
                PromptFact::Integer(value) => ContextValue::Integer(value),
                PromptFact::Number(value) => value.as_str().parse::<i64>().map_or_else(
                    |_| ContextValue::Number(value.as_str().parse().unwrap_or(0.0)),
                    ContextValue::Integer,
                ),
            };
            (key.to_owned(), value)
        })
        .collect::<BTreeMap<_, _>>()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnrichedPlayer {
    pub discord_id: Option<u64>,
    pub hero_id: Option<u16>,
    pub kills: Option<i32>,
    pub deaths: Option<i32>,
    pub assists: Option<i32>,
    pub gpm: Option<i32>,
    pub xpm: Option<i32>,
    pub fantasy_points: Option<f64>,
}

impl EnrichedPlayer {
    fn has_telemetry(&self) -> bool {
        self.hero_id.is_some()
            || self.kills.is_some()
            || self.deaths.is_some()
            || self.assists.is_some()
            || self.gpm.is_some()
            || self.xpm.is_some()
            || self.fantasy_points.is_some()
    }

    fn is_extreme_loser(&self) -> bool {
        let deaths = self.deaths.unwrap_or(0);
        deaths >= 14
            || (deaths >= 8
                && f64::from(self.kills.unwrap_or(0) + self.assists.unwrap_or(0))
                    / f64::from(deaths.max(1))
                    <= 0.75)
    }

    fn winner_selection_key(&self) -> (OrderedFloat, i32, i32) {
        (
            OrderedFloat(self.fantasy_points.unwrap_or(0.0)),
            self.kills.unwrap_or(0) + self.assists.unwrap_or(0),
            self.gpm.unwrap_or(0),
        )
    }

    fn loser_selection_key(&self) -> (i32, i32) {
        (
            self.deaths.unwrap_or(0),
            -(self.kills.unwrap_or(0) + self.assists.unwrap_or(0)),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OrderedFloat(f64);

impl Eq for OrderedFloat {}

impl PartialOrd for OrderedFloat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedFloat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PetFeed {
    pub pet_name: String,
    pub species_id: String,
    pub fullness_gain: i64,
}

pub trait PetMatchPort {
    fn on_match_win(
        &mut self,
        winning_player_ids: &[u64],
        guild_id: Option<u64>,
    ) -> Result<Vec<PetFeed>, RuntimePortError>;
}

pub trait MatchDeliveryPort {
    fn send_public_text(&mut self, content: String) -> Result<(), RuntimePortError>;
    fn send_neon(&mut self, result: NeonResult) -> Result<(), RuntimePortError>;
}

pub fn run_pet_match_hooks<P, D>(
    pets: &mut P,
    delivery: &mut D,
    guild_id: Option<u64>,
    winning_player_ids: &[u64],
) where
    P: PetMatchPort,
    D: MatchDeliveryPort,
{
    if winning_player_ids.is_empty() {
        return;
    }
    let Ok(feeds) = pets.on_match_win(winning_player_ids, guild_id) else {
        return;
    };
    let Some(content) = pet_snack_announcement(&feeds) else {
        return;
    };
    let _ignored = delivery.send_public_text(content);
}

#[must_use]
pub fn pet_snack_announcement(feeds: &[PetFeed]) -> Option<String> {
    if feeds.is_empty() {
        return None;
    }
    let lines = feeds
        .iter()
        .take(PET_ANNOUNCEMENT_LIMIT)
        .map(|feed| {
            let snack = if feed.fullness_gain == 0 {
                "a victory snack — already full".to_owned()
            } else {
                format!("+{} fullness in celebration", feed.fullness_gain)
            };
            format!(
                "🦙 **{}** the {} munches {snack}",
                feed.pet_name,
                get_species(&feed.species_id).display_name,
            )
        })
        .collect::<Vec<_>>();
    Some(format!("🎉 Post-game snacks:\n{}", lines.join("\n")))
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SettlementWinner {
    pub discord_id: u64,
    pub amount: i64,
    pub payout: i64,
    pub refunded: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettlementLoser {
    pub discord_id: u64,
    pub amount: i64,
    pub effective_bet: Option<i64>,
    pub leverage: i64,
    pub balance_after: Option<i64>,
    pub refunded: bool,
}

impl Default for SettlementLoser {
    fn default() -> Self {
        Self {
            discord_id: 0,
            amount: 0,
            effective_bet: None,
            leverage: 1,
            balance_after: None,
            refunded: false,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NotableStreak {
    pub discord_id: u64,
    pub streak: i64,
    pub is_win: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MatchRouteRecord {
    pub match_id: i64,
    pub winning_player_ids: Vec<u64>,
    pub notable_streak: Option<NotableStreak>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RatingHistoryEntry {
    pub discord_id: u64,
    pub rating_before: Option<f64>,
    pub rating_after: Option<f64>,
    pub openskill_display_change: Option<f64>,
    pub expected_win_prob: Option<f64>,
}

pub trait MatchRatingPort {
    fn rating_history(
        &mut self,
        match_id: i64,
    ) -> Result<Vec<RatingHistoryEntry>, RuntimePortError>;
}

pub trait MatchBalancePort {
    fn get_balances(
        &mut self,
        discord_ids: &[u64],
        guild_id: Option<u64>,
    ) -> Result<BTreeMap<u64, i64>, RuntimePortError>;

    fn get_balance(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
    ) -> Result<i64, RuntimePortError>;
}

pub trait MatchNeonPort {
    fn on_big_win(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        payout: i64,
        flavor: BigWinFlavor,
    ) -> Option<NeonResult>;

    fn on_leverage_loss(
        &mut self,
        _discord_id: u64,
        _guild_id: Option<u64>,
        _amount: i64,
        _leverage: i64,
        _new_balance: i64,
    ) -> Option<NeonResult> {
        None
    }

    fn on_bet_settled(
        &mut self,
        _discord_id: u64,
        _guild_id: Option<u64>,
        _new_balance: i64,
    ) -> Option<NeonResult> {
        None
    }

    fn degen_scores(&mut self, discord_ids: &[u64], guild_id: Option<u64>) -> BTreeMap<u64, i64>;

    fn on_degen_milestone(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        score: i64,
    ) -> Option<NeonResult>;

    fn on_post_match_debrief(
        &mut self,
        guild_id: Option<u64>,
        context: JopatPostMatchContext,
        winner_id: Option<u64>,
        loser_id: Option<u64>,
    ) -> Option<NeonResult>;
}

pub struct MatchRouteInput<'a> {
    pub guild_id: Option<u64>,
    pub winners: &'a [SettlementWinner],
    pub losers: &'a [SettlementLoser],
    pub record: &'a MatchRouteRecord,
    pub max_debt: i64,
}

pub fn run_neon_match_hooks<N, B, H, D, C>(
    input: MatchRouteInput<'_>,
    neon: &mut N,
    balances: &mut B,
    ratings: &mut H,
    delivery: &mut D,
    choice: &mut C,
) where
    N: MatchNeonPort,
    B: MatchBalancePort,
    H: MatchRatingPort,
    D: MatchDeliveryPort,
    C: JopatChoicePort,
{
    if let Some(top) = input.winners.iter().max_by_key(|winner| winner.payout)
        && top.payout != 0
        && !top.refunded
    {
        let winning_stake = input
            .winners
            .iter()
            .map(|winner| winner.amount)
            .sum::<i64>();
        let losing_stake = input.losers.iter().map(|loser| loser.amount).sum::<i64>();
        let flavor = if winning_stake != 0 && losing_stake > winning_stake {
            BigWinFlavor::Underdog
        } else {
            BigWinFlavor::TopDog
        };
        if send_if_present(
            delivery,
            neon.on_big_win(top.discord_id, input.guild_id, top.payout, flavor),
        ) {
            return;
        }
    }

    let active_losers = input
        .losers
        .iter()
        .filter(|loser| !loser.refunded)
        .collect::<Vec<_>>();
    let loser_ids = unique_ids(active_losers.iter().map(|loser| loser.discord_id));
    let mut balances_by_id = active_losers
        .iter()
        .filter_map(|loser| {
            loser
                .balance_after
                .map(|balance| (loser.discord_id, balance))
        })
        .collect::<BTreeMap<_, _>>();
    let missing = loser_ids
        .iter()
        .copied()
        .filter(|discord_id| !balances_by_id.contains_key(discord_id))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        if let Ok(bulk) = balances.get_balances(&missing, input.guild_id) {
            balances_by_id.extend(bulk);
        }
        for discord_id in missing {
            if !balances_by_id.contains_key(&discord_id)
                && let Ok(balance) = balances.get_balance(discord_id, input.guild_id)
            {
                balances_by_id.insert(discord_id, balance);
            }
        }
    }
    for loser in &active_losers {
        // Zero is itself a callout trigger, so an unknown balance must not
        // default to it -- that would announce a broke player who may not be.
        let Some(new_balance) = balances_by_id.get(&loser.discord_id).copied() else {
            continue;
        };
        let amount = loser.effective_bet.unwrap_or(loser.amount);
        if loser.leverage >= 5
            && new_balance < 0
            && send_if_present(
                delivery,
                neon.on_leverage_loss(
                    loser.discord_id,
                    input.guild_id,
                    amount,
                    loser.leverage,
                    new_balance,
                ),
            )
        {
            return;
        }
        if (new_balance <= -input.max_debt || new_balance == 0)
            && send_if_present(
                delivery,
                neon.on_bet_settled(loser.discord_id, input.guild_id, new_balance),
            )
        {
            return;
        }
    }

    if !active_losers.is_empty() {
        let scores = neon.degen_scores(&loser_ids, input.guild_id);
        for loser in &active_losers {
            if let Some(score) = scores.get(&loser.discord_id).copied()
                && score >= DEGEN_MILESTONE_SCORE
                && send_if_present(
                    delivery,
                    neon.on_degen_milestone(loser.discord_id, input.guild_id, score),
                )
            {
                return;
            }
        }
    }

    let context =
        select_debrief_candidate(input.winners, &active_losers, input.record, ratings, choice);
    let result = neon.on_post_match_debrief(
        input.guild_id,
        context.context,
        context.winner_id,
        context.loser_id,
    );
    let _sent = send_if_present(delivery, result);
}

fn send_if_present<D>(delivery: &mut D, result: Option<NeonResult>) -> bool
where
    D: MatchDeliveryPort,
{
    result.is_some_and(|result| delivery.send_neon(result).is_ok())
}

fn unique_ids(ids: impl IntoIterator<Item = u64>) -> Vec<u64> {
    ids.into_iter().fold(Vec::new(), |mut unique, id| {
        if !unique.contains(&id) {
            unique.push(id);
        }
        unique
    })
}

#[derive(Clone, Debug, PartialEq)]
struct DebriefCandidate {
    context: JopatPostMatchContext,
    winner_id: Option<u64>,
    loser_id: Option<u64>,
}

fn select_debrief_candidate<H, C>(
    winners: &[SettlementWinner],
    active_losers: &[&SettlementLoser],
    record: &MatchRouteRecord,
    ratings: &mut H,
    choice: &mut C,
) -> DebriefCandidate
where
    H: MatchRatingPort,
    C: JopatChoicePort,
{
    let mut candidates = Vec::new();
    if let Some(winner) = winners.iter().max_by_key(|winner| winner.payout) {
        candidates.push(DebriefCandidate {
            context: JopatPostMatchContext {
                payout: winner.payout,
                ..JopatPostMatchContext::default()
            },
            winner_id: Some(winner.discord_id),
            loser_id: None,
        });
    }
    if let Some(loser) = active_losers.iter().copied().max_by_key(|loser| {
        (
            loser.leverage.max(1),
            loser.effective_bet.unwrap_or(loser.amount),
        )
    }) {
        candidates.push(DebriefCandidate {
            context: JopatPostMatchContext {
                loss: loser.effective_bet.unwrap_or(loser.amount),
                leverage: loser.leverage.max(1),
                ..JopatPostMatchContext::default()
            },
            winner_id: None,
            loser_id: Some(loser.discord_id),
        });
    }
    if let Some(streak) = &record.notable_streak
        && streak.streak != 0
    {
        let signed_streak = if streak.is_win {
            streak.streak
        } else {
            -streak.streak.abs()
        };
        candidates.push(DebriefCandidate {
            context: JopatPostMatchContext {
                streak: signed_streak,
                ..JopatPostMatchContext::default()
            },
            winner_id: (signed_streak > 0).then_some(streak.discord_id),
            loser_id: (signed_streak < 0).then_some(streak.discord_id),
        });
    }
    if let Some((winner_id, rating_change, expected_win_prob)) =
        notable_winner(record, ratings, choice)
    {
        candidates.push(DebriefCandidate {
            context: JopatPostMatchContext {
                rating_change: rating_change.map(NumericFact::from),
                expected_win_prob: expected_win_prob.map(NumericFact::from),
                ..JopatPostMatchContext::default()
            },
            winner_id: Some(winner_id),
            loser_id: None,
        });
    }
    if candidates.is_empty() {
        candidates.push(DebriefCandidate {
            context: JopatPostMatchContext::default(),
            winner_id: None,
            loser_id: None,
        });
    }
    let mut best = candidates.remove(0);
    let mut best_chance = post_match_chance(&best.context);
    for candidate in candidates {
        let chance = post_match_chance(&candidate.context);
        if chance > best_chance {
            best = candidate;
            best_chance = chance;
        }
    }
    best
}

fn notable_winner<H, C>(
    record: &MatchRouteRecord,
    ratings: &mut H,
    choice: &mut C,
) -> Option<(u64, Option<f64>, Option<f64>)>
where
    H: MatchRatingPort,
    C: JopatChoicePort,
{
    if record.winning_player_ids.is_empty() {
        return None;
    }
    let history = ratings.rating_history(record.match_id).unwrap_or_default();
    let winners = history
        .iter()
        .filter(|entry| record.winning_player_ids.contains(&entry.discord_id))
        .collect::<Vec<_>>();
    if winners.is_empty() {
        let index = choice.choose_index(record.winning_player_ids.len());
        return Some((record.winning_player_ids[index], None, None));
    }
    let underdog = winners
        .iter()
        .copied()
        .min_by(|left, right| {
            left.expected_win_prob
                .unwrap_or(0.5)
                .total_cmp(&right.expected_win_prob.unwrap_or(0.5))
        })
        .expect("non-empty rating winners");
    if underdog.expected_win_prob.unwrap_or(0.5) < 0.45 {
        return Some((
            underdog.discord_id,
            strongest_rating_change(underdog),
            underdog.expected_win_prob,
        ));
    }
    let best = winners
        .into_iter()
        .max_by(|left, right| {
            strongest_rating_change(left)
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&strongest_rating_change(right).unwrap_or(f64::NEG_INFINITY))
        })
        .expect("non-empty rating winners");
    Some((
        best.discord_id,
        strongest_rating_change(best),
        best.expected_win_prob,
    ))
}

fn strongest_rating_change(entry: &RatingHistoryEntry) -> Option<f64> {
    [
        entry
            .rating_before
            .zip(entry.rating_after)
            .map(|(before, after)| after - before),
        entry.openskill_display_change,
    ]
    .into_iter()
    .flatten()
    .max_by(f64::total_cmp)
}

#[cfg(test)]
#[path = "jopat_match_routing/tests.rs"]
mod tests;
