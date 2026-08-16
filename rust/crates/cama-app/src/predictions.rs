//! Discord-independent prediction-market policy and command orchestration.
//!
//! The live Discord adapter is intentionally absent while Python remains the
//! serving process. This module ports validation, ladder construction,
//! refresh math, guild/admin guards, settlement copy, chunking, attachment
//! fallback, and thread lifecycle decisions into typed values that an eventual
//! Serenity/Twilight adapter can execute without changing policy.

use cama_db::predictions_repository::{
    Book, ContractSide, Market, NewLevel, PredictionRepository, PredictionRepositoryError,
    Settlement,
};
use thiserror::Error;

pub const LEVELS_PER_SIDE: usize = 3;
pub const SIZE_PER_LEVEL: i64 = 50;
pub const OUTER_LEVEL_SIZES: [i64; 4] = [20, 15, 10, 5];
pub const SPREAD_TICKS: i64 = 2;
pub const REFRESH_LEVELS_PER_SIDE: usize = 3;
pub const REFRESH_SIZE_PER_LEVEL: i64 = 10;
pub const REFRESH_OUTER_LEVEL_SIZES: [i64; 4] = [8, 6, 4, 2];
pub const REFRESH_SPREAD_TICKS: i64 = 4;
pub const DRIFT_MIN: i64 = -3;
pub const DRIFT_MAX: i64 = 3;
pub const FADE_TICKS: i64 = 5;
pub const PRICE_LOW: i64 = 5;
pub const PRICE_HIGH: i64 = 95;
pub const THREAD_AUTO_ARCHIVE_MINUTES: i64 = 10_080;
pub const DISCORD_MESSAGE_LIMIT: usize = 2_000;

#[derive(Debug, Error)]
pub enum PredictionServiceError {
    #[error("Question must be at least 5 characters.")]
    QuestionTooShort,
    #[error("initial_fair must be in [{PRICE_LOW}, {PRICE_HIGH}].")]
    InvalidInitialFair,
    #[error("new_price must be in [{PRICE_LOW}, {PRICE_HIGH}].")]
    InvalidNewPrice,
    #[error("Prediction not found.")]
    PredictionNotFound,
    #[error("Cannot set fair on a {0} market.")]
    CannotSetFair(String),
    #[error("drift must be in [{DRIFT_MIN}, {DRIFT_MAX}]")]
    InvalidDrift,
    #[error(transparent)]
    Repository(#[from] PredictionRepositoryError),
}

#[derive(Clone, Debug)]
pub struct PredictionService {
    repository: PredictionRepository,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateMarketResult {
    pub prediction_id: i64,
    pub question: String,
    pub creator_id: i64,
    pub initial_fair: i64,
    pub current_price: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefreshResult {
    pub prediction_id: i64,
    pub old_price: i64,
    pub new_price: i64,
    pub drift: i64,
    pub skipped: bool,
}

impl PredictionService {
    #[must_use]
    pub const fn new(repository: PredictionRepository) -> Self {
        Self { repository }
    }

    #[must_use]
    pub const fn repository(&self) -> &PredictionRepository {
        &self.repository
    }

    pub fn create_orderbook_prediction(
        &self,
        guild_id: Option<i64>,
        creator_id: i64,
        question: &str,
        initial_fair: Option<i64>,
        channel_id: Option<i64>,
    ) -> Result<CreateMarketResult, PredictionServiceError> {
        let question = question.trim();
        if question.len() < 5 {
            return Err(PredictionServiceError::QuestionTooShort);
        }
        let initial_fair = initial_fair.unwrap_or(50);
        if !(PRICE_LOW..=PRICE_HIGH).contains(&initial_fair) {
            return Err(PredictionServiceError::InvalidInitialFair);
        }
        let levels = build_initial_levels(initial_fair);
        let prediction_id = self.repository.create_orderbook_prediction(
            guild_id,
            creator_id,
            question,
            initial_fair,
            channel_id,
            &levels,
        )?;
        Ok(CreateMarketResult {
            prediction_id,
            question: question.to_owned(),
            creator_id,
            initial_fair,
            current_price: initial_fair,
        })
    }

    pub fn refresh_market_with_drift(
        &self,
        prediction_id: i64,
        drift: i64,
        now: i64,
    ) -> Result<RefreshResult, PredictionServiceError> {
        if !(DRIFT_MIN..=DRIFT_MAX).contains(&drift) {
            return Err(PredictionServiceError::InvalidDrift);
        }
        let Some(market) = self.repository.get_prediction(prediction_id)? else {
            return Ok(RefreshResult {
                prediction_id,
                old_price: 50,
                new_price: 50,
                drift,
                skipped: true,
            });
        };
        if market.status != "open" {
            return Ok(RefreshResult {
                prediction_id,
                old_price: market.current_price.unwrap_or(50),
                new_price: market.current_price.unwrap_or(50),
                drift,
                skipped: true,
            });
        }
        let old_price = market.current_price.unwrap_or(50);
        let book = self.repository.get_book(prediction_id)?;
        let observed = observed_mid(&self.repository, &market, &book)?;
        let new_price = clamp_price(python_round(observed) + drift);
        let levels = build_refresh_levels(new_price);
        self.repository.apply_refresh(
            prediction_id,
            new_price,
            &levels,
            now,
            "refresh",
            SPREAD_TICKS,
        )?;
        Ok(RefreshResult {
            prediction_id,
            old_price,
            new_price,
            drift,
            skipped: false,
        })
    }

    pub fn set_fair_manual(
        &self,
        prediction_id: i64,
        new_price: i64,
        now: i64,
    ) -> Result<RefreshResult, PredictionServiceError> {
        if !(PRICE_LOW..=PRICE_HIGH).contains(&new_price) {
            return Err(PredictionServiceError::InvalidNewPrice);
        }
        let market = self
            .repository
            .get_prediction(prediction_id)?
            .ok_or(PredictionServiceError::PredictionNotFound)?;
        validate_set_fair(&market.status, new_price)?;
        let old_price = market.current_price.unwrap_or(50);
        self.repository.apply_refresh(
            prediction_id,
            new_price,
            &build_initial_levels(new_price),
            now,
            "set_fair",
            SPREAD_TICKS,
        )?;
        Ok(RefreshResult {
            prediction_id,
            old_price,
            new_price,
            drift: 0,
            skipped: false,
        })
    }
}

pub fn validate_set_fair(
    market_status: &str,
    new_price: i64,
) -> Result<(), PredictionServiceError> {
    if !(PRICE_LOW..=PRICE_HIGH).contains(&new_price) {
        return Err(PredictionServiceError::InvalidNewPrice);
    }
    if market_status != "open" {
        return Err(PredictionServiceError::CannotSetFair(
            market_status.to_owned(),
        ));
    }
    Ok(())
}

#[must_use]
pub fn build_initial_levels(fair: i64) -> Vec<NewLevel> {
    build_levels(
        fair,
        LEVELS_PER_SIDE,
        SIZE_PER_LEVEL,
        SPREAD_TICKS,
        &OUTER_LEVEL_SIZES,
    )
}

#[must_use]
pub fn build_refresh_levels(fair: i64) -> Vec<NewLevel> {
    build_levels(
        fair,
        REFRESH_LEVELS_PER_SIDE,
        REFRESH_SIZE_PER_LEVEL,
        REFRESH_SPREAD_TICKS,
        &REFRESH_OUTER_LEVEL_SIZES,
    )
}

fn build_levels(
    fair: i64,
    normal_levels: usize,
    size: i64,
    spread: i64,
    outer_sizes: &[i64],
) -> Vec<NewLevel> {
    let mut sizes = vec![size; normal_levels];
    sizes.extend(outer_sizes.iter().copied().map(|value| value.max(0)));
    let mut result = Vec::with_capacity(sizes.len() * 2);
    for (index, level_size) in sizes.into_iter().enumerate() {
        if level_size == 0 {
            continue;
        }
        let offset = spread + i64::try_from(index).expect("prediction ladder index fits i64");
        let ask = fair + offset;
        let bid = fair - offset;
        if (1..=99).contains(&ask) {
            result.push(NewLevel::ask(ask, level_size));
        }
        if (1..=99).contains(&bid) {
            result.push(NewLevel::bid(bid, level_size));
        }
    }
    result
}

fn observed_mid(
    repository: &PredictionRepository,
    market: &Market,
    book: &Book,
) -> Result<f64, PredictionRepositoryError> {
    let since = market.last_refresh_at.unwrap_or(0);
    match (book.yes_asks.first(), book.yes_bids.first()) {
        (Some(ask), Some(bid)) => Ok((ask.0 + bid.0) as f64 / 2.0),
        (None, Some(bid)) => Ok(repository
            .last_fill_price_since(market.prediction_id, &["buy_yes", "sell_no"], since)?
            .unwrap_or(bid.0) as f64
            + FADE_TICKS as f64),
        (Some(ask), None) => Ok(repository
            .last_fill_price_since(market.prediction_id, &["buy_no", "sell_yes"], since)?
            .unwrap_or(ask.0) as f64
            - FADE_TICKS as f64),
        (None, None) => {
            let lifted = repository.last_fill_price_since(
                market.prediction_id,
                &["buy_yes", "sell_no"],
                since,
            )?;
            let hit = repository.last_fill_price_since(
                market.prediction_id,
                &["buy_no", "sell_yes"],
                since,
            )?;
            Ok(match (lifted, hit) {
                (Some(ask), Some(bid)) => (ask + bid) as f64 / 2.0,
                (Some(ask), None) => (ask + FADE_TICKS) as f64,
                (None, Some(bid)) => (bid - FADE_TICKS) as f64,
                (None, None) => market.current_price.unwrap_or(50) as f64,
            })
        }
    }
}

/// Pure refresh target used by the repository-backed service and parity tests.
#[must_use]
pub fn refresh_target(
    book: &Book,
    old_price: i64,
    last_lifted_ask: Option<i64>,
    last_hit_bid: Option<i64>,
    drift: i64,
) -> i64 {
    let observed = match (book.yes_asks.first(), book.yes_bids.first()) {
        (Some(ask), Some(bid)) => (ask.0 + bid.0) as f64 / 2.0,
        (None, Some(bid)) => (last_lifted_ask.unwrap_or(bid.0) + FADE_TICKS) as f64,
        (Some(ask), None) => (last_hit_bid.unwrap_or(ask.0) - FADE_TICKS) as f64,
        (None, None) => match (last_lifted_ask, last_hit_bid) {
            (Some(ask), Some(bid)) => (ask + bid) as f64 / 2.0,
            (Some(ask), None) => (ask + FADE_TICKS) as f64,
            (None, Some(bid)) => (bid - FADE_TICKS) as f64,
            (None, None) => old_price as f64,
        },
    };
    clamp_price(python_round(observed) + drift)
}

fn python_round(value: f64) -> i64 {
    let floor = value.floor();
    let fraction = value - floor;
    if (fraction - 0.5).abs() < f64::EPSILON {
        let floor = floor as i64;
        if floor % 2 == 0 { floor } else { floor + 1 }
    } else {
        value.round() as i64
    }
}

#[must_use]
pub const fn clamp_price(price: i64) -> i64 {
    if price < PRICE_LOW {
        PRICE_LOW
    } else if price > PRICE_HIGH {
        PRICE_HIGH
    } else {
        price
    }
}

#[must_use]
pub fn position_mark(book: &Book, side: ContractSide) -> Option<i64> {
    match side {
        ContractSide::Yes => book.yes_bids.first().map(|level| level.0),
        ContractSide::No => book.yes_asks.first().map(|level| 100 - level.0),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Ephemeral,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandReply {
    pub content: String,
    pub visibility: Visibility,
    pub attachment: Option<ChartAttachment>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChartAttachment {
    pub filename: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdminMarketCommand {
    Cancel,
    ForceRefresh,
    Resolve,
    Rollback,
    SetFair,
    View,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandGate {
    Allowed,
    Rejected(CommandReply),
}

#[must_use]
pub fn gate_admin_market_command(
    is_admin: bool,
    interaction_guild_id: Option<i64>,
    market: Option<&Market>,
    command: AdminMarketCommand,
) -> CommandGate {
    if !is_admin && command != AdminMarketCommand::View {
        return CommandGate::Rejected(CommandReply {
            content: match command {
                AdminMarketCommand::Rollback => "Only admins can roll back markets.".to_owned(),
                _ => "Only admins can manage prediction markets.".to_owned(),
            },
            visibility: Visibility::Ephemeral,
            attachment: None,
        });
    }
    let expected_guild = interaction_guild_id.unwrap_or(0);
    if market.is_none_or(|market| market.guild_id != expected_guild) {
        return CommandGate::Rejected(CommandReply {
            content: "Market not found in this server.".to_owned(),
            visibility: Visibility::Ephemeral,
            attachment: None,
        });
    }
    CommandGate::Allowed
}

#[must_use]
pub fn force_refresh_reply(result: &RefreshResult) -> CommandReply {
    let content = if result.skipped {
        "Market refresh skipped: market is not open.".to_owned()
    } else {
        format!(
            "Market #{} manually refreshed: {}% → {}%.",
            result.prediction_id, result.old_price, result.new_price
        )
    };
    CommandReply {
        content,
        visibility: Visibility::Public,
        attachment: None,
    }
}

#[must_use]
pub fn rollback_reply(
    prediction_id: i64,
    previous_outcome: ContractSide,
    total_reversed: i64,
    affected_players: i64,
) -> CommandReply {
    let outcome = match previous_outcome {
        ContractSide::Yes => "YES",
        ContractSide::No => "NO",
    };
    CommandReply {
        content: format!(
            "Market #{prediction_id} {outcome} resolution rolled back: {total_reversed} JC reversed across {affected_players} players."
        ),
        visibility: Visibility::Public,
        attachment: None,
    }
}

#[must_use]
pub fn refresh_status_markets(markets: &[Market], guild_id: Option<i64>) -> Vec<Market> {
    let guild_id = guild_id.unwrap_or(0);
    markets
        .iter()
        .filter(|market| market.guild_id == guild_id && market.status == "open")
        .cloned()
        .collect()
}

#[must_use]
pub fn neutralize_discord_mentions(question: &str) -> String {
    resolve_and_neutralize_discord_mentions(question, |_| None, |_| None)
}

#[must_use]
pub fn resolve_and_neutralize_discord_mentions<U, R>(
    question: &str,
    user_name: U,
    role_name: R,
) -> String
where
    U: Fn(i64) -> Option<String>,
    R: Fn(i64) -> Option<String>,
{
    let mut resolved = String::with_capacity(question.len());
    let mut cursor = 0;
    while cursor < question.len() {
        let suffix = &question[cursor..];
        let (prefix_len, role) = if suffix.starts_with("<@&") {
            (3, true)
        } else if suffix.starts_with("<@") {
            (2, false)
        } else {
            let character = suffix.chars().next().expect("cursor is in bounds");
            resolved.push(character);
            cursor += character.len_utf8();
            continue;
        };
        let Some(close_offset) = suffix.find('>') else {
            resolved.push_str(suffix);
            break;
        };
        let raw_id = &suffix[prefix_len..close_offset];
        let raw_id = if role {
            raw_id
        } else {
            raw_id.strip_prefix('!').unwrap_or(raw_id)
        };
        let Ok(discord_id) = raw_id.parse::<i64>() else {
            resolved.push_str(&suffix[..=close_offset]);
            cursor += close_offset + 1;
            continue;
        };
        let name = if role {
            role_name(discord_id)
        } else {
            user_name(discord_id)
        }
        .unwrap_or_else(|| raw_id.to_owned());
        resolved.push('@');
        resolved.push_str(&name);
        cursor += close_offset + 1;
    }
    resolved
        .replace("@everyone", "@\u{200b}everyone")
        .replace("@here", "@\u{200b}here")
}

#[must_use]
pub fn settlement_lines(settlement: &Settlement) -> Vec<String> {
    settlement
        .participants
        .iter()
        .map(|participant| {
            format!(
                "<@{}> spent {} ({} yes / {} no) → won {} JC (net {:+})",
                participant.discord_id,
                participant.cost_basis,
                participant.yes_contracts,
                participant.no_contracts,
                participant.payout,
                participant.profit
            )
        })
        .collect()
}

#[must_use]
pub fn resolution_announcement_chunks(
    prediction_id: i64,
    question: &str,
    outcome: ContractSide,
    participant_lines: &[String],
) -> Vec<String> {
    let outcome = match outcome {
        ContractSide::Yes => "YES",
        ContractSide::No => "NO",
    };
    let headline = format!(
        "Market #{prediction_id} resolved {outcome}: \"{}\"",
        neutralize_discord_mentions(question)
    );
    chunk_message_lines(&headline, participant_lines, DISCORD_MESSAGE_LIMIT)
}

#[must_use]
pub fn chunk_message_lines(header: &str, lines: &[String], max_len: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = header.to_owned();
    for line in lines {
        if current.len() + 1 + line.len() > max_len && !current.is_empty() {
            chunks.push(current);
            current = String::new();
        }
        if !current.is_empty() {
            current.push('\n');
        }
        if line.len() <= max_len {
            current.push_str(line);
        } else {
            current.extend(line.chars().take(max_len));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnnouncementSurface {
    CommandReply,
    MarketThread,
    GambaChannel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnouncementSend {
    pub surface: AnnouncementSurface,
    pub content: String,
    pub include_chart: bool,
}

/// Plan complete text for every surface. An adapter retries the same send with
/// `include_chart=false` when Discord rejects an attachment.
#[must_use]
pub fn resolution_publish_plan(chunks: &[String], chart_available: bool) -> Vec<AnnouncementSend> {
    let surfaces = [
        AnnouncementSurface::CommandReply,
        AnnouncementSurface::MarketThread,
        AnnouncementSurface::GambaChannel,
    ];
    surfaces
        .into_iter()
        .flat_map(|surface| {
            chunks
                .iter()
                .enumerate()
                .map(move |(index, content)| AnnouncementSend {
                    surface,
                    content: content.clone(),
                    include_chart: chart_available && index == 0,
                })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentResult {
    Sent,
    Forbidden,
}

#[must_use]
pub fn retry_without_attachment(
    attempted: &AnnouncementSend,
    result: AttachmentResult,
) -> Option<AnnouncementSend> {
    if result == AttachmentResult::Forbidden && attempted.include_chart {
        Some(AnnouncementSend {
            include_chart: false,
            ..attempted.clone()
        })
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadState {
    pub archived: bool,
    pub locked: bool,
    pub message_exists: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadAction {
    FetchMessage,
    Unarchive { auto_archive_minutes: i64 },
    EditEmbed { restore_view: bool },
    LockAndArchive,
}

#[must_use]
pub fn refresh_embed_plan(state: ThreadState, restore_view: bool) -> Vec<ThreadAction> {
    let mut actions = vec![ThreadAction::FetchMessage];
    if !state.message_exists {
        return actions;
    }
    if state.archived {
        actions.push(ThreadAction::Unarchive {
            auto_archive_minutes: THREAD_AUTO_ARCHIVE_MINUTES,
        });
    }
    actions.push(ThreadAction::EditEmbed { restore_view });
    actions
}

#[must_use]
pub fn resolved_thread_plan(state: ThreadState) -> Vec<ThreadAction> {
    let mut actions = refresh_embed_plan(state, false);
    if state.message_exists {
        actions.push(ThreadAction::LockAndArchive);
    }
    actions
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupBranch {
    Confirmation,
    Announcement,
    Chart,
    Thread,
    Embed,
    Pin,
    Persist,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateSetupState {
    pub confirmation_done: bool,
    pub confirmation_error: Option<String>,
    pub announcement_done: bool,
    pub announcement_error: Option<String>,
    pub chart_done: bool,
    pub thread_done: bool,
    pub embed_done: bool,
    pub pinned: bool,
    pub persisted: bool,
}

impl CreateSetupState {
    #[must_use]
    pub const fn can_start_thread(&self) -> bool {
        self.announcement_done && self.announcement_error.is_none()
    }

    #[must_use]
    pub const fn can_send_embed(&self) -> bool {
        self.thread_done && self.chart_done
    }

    #[must_use]
    pub const fn can_persist(&self) -> bool {
        self.embed_done && self.pinned
    }

    #[must_use]
    pub const fn can_return(&self) -> bool {
        self.confirmation_done
            && self.chart_done
            && (self.announcement_done || self.announcement_error.is_some())
            && (self.persisted || self.announcement_error.is_some())
    }

    pub fn complete(&mut self, branch: SetupBranch) {
        match branch {
            SetupBranch::Confirmation => self.confirmation_done = true,
            SetupBranch::Announcement => self.announcement_done = true,
            SetupBranch::Chart => self.chart_done = true,
            SetupBranch::Thread => self.thread_done = true,
            SetupBranch::Embed => self.embed_done = true,
            SetupBranch::Pin => self.pinned = true,
            SetupBranch::Persist => self.persisted = true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChartSpec {
    pub market_id: i64,
    pub created_at: i64,
    pub snapshots: Vec<(i64, i64)>,
    pub y_min: i64,
    pub y_max: i64,
}

#[must_use]
pub fn chart_spec(market_id: i64, created_at: i64, snapshots: &[(i64, i64)]) -> Option<ChartSpec> {
    if created_at < 0 || snapshots.iter().any(|(_, fair)| !(0..=100).contains(fair)) {
        return None;
    }
    let (minimum, maximum) = snapshots
        .iter()
        .fold((50_i64, 50_i64), |(minimum, maximum), (_, fair)| {
            (minimum.min(*fair), maximum.max(*fair))
        });
    let padding = ((maximum - minimum) / 5).max(5);
    Some(ChartSpec {
        market_id,
        created_at,
        snapshots: snapshots.to_vec(),
        y_min: (minimum - padding).max(0),
        y_max: (maximum + padding).min(100),
    })
}

/// Stable adapter-neutral bytes used by tests and cache keys. A live image
/// renderer will consume `ChartSpec`; malformed metadata remains best effort.
#[must_use]
pub fn chart_fingerprint(spec: &ChartSpec) -> Vec<u8> {
    format!(
        "prediction:{}:{}:{}:{}:{:?}",
        spec.market_id, spec.created_at, spec.y_min, spec.y_max, spec.snapshots
    )
    .into_bytes()
}

#[must_use]
pub fn build_chart_attachment(
    spec: Option<&ChartSpec>,
    construction_succeeds: bool,
) -> Option<ChartAttachment> {
    if !construction_succeeds {
        return None;
    }
    spec.map(|spec| ChartAttachment {
        filename: format!("prediction-{}.png", spec.market_id),
        bytes: chart_fingerprint(spec),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GambaSend {
    pub embed: String,
    pub file: Option<ChartAttachment>,
}

#[must_use]
pub fn gamba_send(embed: &str, file: Option<ChartAttachment>) -> GambaSend {
    GambaSend {
        embed: embed.to_owned(),
        file,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use cama_db::predictions_repository::{
        Book, ContractSide, Market, PredictionRepository, Settlement, SettlementParticipant,
        quote_total,
    };

    use super::{
        AdminMarketCommand, AnnouncementSurface, AttachmentResult, ChartAttachment, CommandGate,
        CreateSetupState, DISCORD_MESSAGE_LIMIT, DRIFT_MAX, DRIFT_MIN, PRICE_HIGH, PRICE_LOW,
        PredictionService, PredictionServiceError, RefreshResult, SetupBranch,
        THREAD_AUTO_ARCHIVE_MINUTES, ThreadAction, ThreadState, Visibility, build_chart_attachment,
        build_initial_levels, build_refresh_levels, chart_fingerprint, chart_spec,
        chunk_message_lines, force_refresh_reply, gamba_send, gate_admin_market_command,
        position_mark, refresh_embed_plan, refresh_status_markets, refresh_target,
        resolution_announcement_chunks, resolution_publish_plan,
        resolve_and_neutralize_discord_mentions, resolved_thread_plan, retry_without_attachment,
        rollback_reply, settlement_lines, validate_set_fair,
    };

    const GUILD: i64 = 101;
    const OTHER_GUILD: i64 = 202;

    fn market(id: i64, guild_id: i64, status: &str) -> Market {
        Market {
            prediction_id: id,
            guild_id,
            creator_id: 999,
            question: format!("Market {id}?"),
            status: status.to_owned(),
            outcome: None,
            current_price: Some(50),
            initial_fair: Some(50),
            prev_price: None,
            last_refresh_at: Some(100),
            lp_pnl: 0,
            created_at: id,
            resolved_at: None,
            resolved_by: None,
            channel_id: Some(11),
            thread_id: Some(555),
            embed_message_id: Some(777),
            channel_message_id: Some(333),
        }
    }

    fn settlement(participants: Vec<SettlementParticipant>) -> Settlement {
        Settlement {
            prediction_id: 42,
            guild_id: GUILD,
            outcome: ContractSide::Yes,
            total_payout: participants
                .iter()
                .map(|participant| participant.payout)
                .sum(),
            lp_pnl: 0,
            participants,
        }
    }

    fn participant(id: i64, yes: i64, no: i64, cost: i64, payout: i64) -> SettlementParticipant {
        SettlementParticipant {
            discord_id: id,
            yes_contracts: yes,
            no_contracts: no,
            winning_contracts: yes,
            cost_basis: cost,
            payout,
            profit: payout - cost,
        }
    }

    #[test]
    fn test_create_orderbook_prediction_populates_ladder() {
        let levels = build_initial_levels(50);
        let asks = levels
            .iter()
            .filter(|level| level.side.as_str() == "yes_ask")
            .map(|level| (level.price, level.size))
            .collect::<Vec<_>>();
        let bids = levels
            .iter()
            .filter(|level| level.side.as_str() == "yes_bid")
            .map(|level| (level.price, level.size))
            .collect::<Vec<_>>();
        assert_eq!(
            asks,
            vec![
                (52, 50),
                (53, 50),
                (54, 50),
                (55, 20),
                (56, 15),
                (57, 10),
                (58, 5)
            ]
        );
        assert_eq!(bids.iter().map(|level| level.1).sum::<i64>(), 200);
    }

    #[test]
    fn test_create_at_fair_bounds_keeps_normal_ladder_and_filters_outer_levels_low() {
        let levels = build_initial_levels(PRICE_LOW);
        let asks = levels
            .iter()
            .filter(|level| level.side.as_str() == "yes_ask")
            .count();
        let bids = levels
            .iter()
            .filter(|level| level.side.as_str() == "yes_bid")
            .map(|level| (level.price, level.size))
            .collect::<Vec<_>>();
        assert_eq!(asks, 7);
        assert_eq!(bids, vec![(3, 50), (2, 50), (1, 50)]);
    }

    #[test]
    fn test_create_at_fair_bounds_keeps_normal_ladder_and_filters_outer_levels_high() {
        let levels = build_initial_levels(PRICE_HIGH);
        let asks = levels
            .iter()
            .filter(|level| level.side.as_str() == "yes_ask")
            .map(|level| (level.price, level.size))
            .collect::<Vec<_>>();
        let bids = levels
            .iter()
            .filter(|level| level.side.as_str() == "yes_bid")
            .count();
        assert_eq!(asks, vec![(97, 50), (98, 50), (99, 50)]);
        assert_eq!(bids, 7);
    }

    #[test]
    fn test_create_rejects_invalid_inputs() {
        let service = PredictionService::new(PredictionRepository::new("/nonexistent/parity.db"));
        assert!(matches!(
            service.create_orderbook_prediction(Some(GUILD), 1, "hi", Some(50), None),
            Err(PredictionServiceError::QuestionTooShort)
        ));
        assert!(matches!(
            service.create_orderbook_prediction(Some(GUILD), 1, "valid question", Some(99), None),
            Err(PredictionServiceError::InvalidInitialFair)
        ));
        assert!(matches!(
            service.create_orderbook_prediction(Some(GUILD), 1, "valid question", Some(0), None),
            Err(PredictionServiceError::InvalidInitialFair)
        ));
    }

    #[test]
    fn test_refresh_uses_widened_drift_bounds() {
        assert_eq!((DRIFT_MIN, DRIFT_MAX), (-3, 3));
        let book = Book {
            current_price: Some(50),
            yes_asks: vec![(52, 50)],
            yes_bids: vec![(48, 50)],
        };
        assert_eq!(refresh_target(&book, 50, None, None, DRIFT_MAX), 53);
    }

    #[test]
    fn test_refresh_keeps_price_in_clamp() {
        let high = Book {
            current_price: Some(95),
            yes_asks: vec![],
            yes_bids: vec![],
        };
        let low = Book {
            current_price: Some(5),
            yes_asks: vec![],
            yes_bids: vec![],
        };
        assert_eq!(refresh_target(&high, 95, None, None, 3), PRICE_HIGH);
        assert_eq!(refresh_target(&low, 5, None, None, -3), PRICE_LOW);
    }

    #[test]
    fn test_refresh_uses_observed_mid_when_book_intact() {
        let book = Book {
            current_price: Some(70),
            yes_asks: vec![(52, 1)],
            yes_bids: vec![(48, 1)],
        };
        assert_eq!(refresh_target(&book, 70, None, None, 0), 50);
    }

    #[test]
    fn test_refresh_fades_farther_up_when_asks_consumed() {
        let book = Book {
            current_price: Some(50),
            yes_asks: vec![],
            yes_bids: vec![(48, 1)],
        };
        assert_eq!(refresh_target(&book, 50, Some(57), None, 0), 62);
    }

    #[test]
    fn test_refresh_fades_farther_down_when_bids_consumed() {
        let book = Book {
            current_price: Some(50),
            yes_asks: vec![(52, 1)],
            yes_bids: vec![],
        };
        assert_eq!(refresh_target(&book, 50, None, Some(43), 0), 38);
    }

    #[test]
    fn test_refresh_uses_terminal_fill_midpoint_when_both_sides_consumed() {
        let book = Book {
            current_price: Some(50),
            yes_asks: vec![],
            yes_bids: vec![],
        };
        assert_eq!(refresh_target(&book, 50, Some(61), Some(42), 0), 52);
    }

    #[test]
    fn test_set_fair_manual_changes_price_and_layers_ladder() {
        let levels = build_initial_levels(70);
        assert!(
            levels
                .iter()
                .any(|level| level.side.as_str() == "yes_ask" && level.price == 72)
        );
        assert!(
            levels
                .iter()
                .any(|level| level.side.as_str() == "yes_bid" && level.price == 68)
        );
    }

    #[test]
    fn test_refresh_layers_with_refresh_params_not_initial() {
        let refresh = build_refresh_levels(50);
        let asks = refresh
            .iter()
            .filter(|level| level.side.as_str() == "yes_ask")
            .map(|level| (level.price, level.size))
            .collect::<Vec<_>>();
        assert_eq!(
            asks,
            vec![
                (54, 10),
                (55, 10),
                (56, 10),
                (57, 8),
                (58, 6),
                (59, 4),
                (60, 2)
            ]
        );
        assert_ne!(refresh, build_initial_levels(50));
    }

    #[test]
    fn test_set_fair_manual_rejects_out_of_range_and_resolved() {
        let service = PredictionService::new(PredictionRepository::new("/nonexistent/parity.db"));
        assert!(matches!(
            service.set_fair_manual(1, 99, 100),
            Err(PredictionServiceError::InvalidNewPrice)
        ));
        assert!(matches!(
            service.set_fair_manual(1, 2, 100),
            Err(PredictionServiceError::InvalidNewPrice)
        ));
        assert!(matches!(
            validate_set_fair("resolved", 60),
            Err(PredictionServiceError::CannotSetFair(status)) if status == "resolved"
        ));
    }

    #[test]
    fn test_position_mark_helper() {
        let book = Book {
            current_price: Some(50),
            yes_asks: vec![(55, 5), (56, 5)],
            yes_bids: vec![(45, 5), (44, 5)],
        };
        assert_eq!(position_mark(&book, ContractSide::Yes), Some(45));
        assert_eq!(position_mark(&book, ContractSide::No), Some(45));
        let one_sided = Book {
            yes_asks: vec![],
            ..book
        };
        assert_eq!(position_mark(&one_sided, ContractSide::No), None);
    }

    #[test]
    fn test_force_refresh_admin_only() {
        assert!(
            matches!(gate_admin_market_command(false, Some(GUILD), Some(&market(1, GUILD, "open")),
            AdminMarketCommand::ForceRefresh), CommandGate::Rejected(reply) if reply.visibility == Visibility::Ephemeral && reply.content.contains("admin"))
        );
    }

    #[test]
    fn test_force_refresh_announces_and_posts_to_thread() {
        let reply = force_refresh_reply(&RefreshResult {
            prediction_id: 7,
            old_price: 50,
            new_price: 53,
            drift: 3,
            skipped: false,
        });
        assert!(reply.content.contains("manually refreshed"));
        let sends = resolution_publish_plan(std::slice::from_ref(&reply.content), false);
        assert!(
            sends
                .iter()
                .any(|send| send.surface == AnnouncementSurface::MarketThread)
        );
    }

    #[test]
    fn test_force_refresh_skipped_when_resolved() {
        let reply = force_refresh_reply(&RefreshResult {
            prediction_id: 7,
            old_price: 50,
            new_price: 50,
            drift: 0,
            skipped: true,
        });
        assert!(reply.content.to_lowercase().contains("skipped"));
    }

    macro_rules! cross_guild_case {
        ($name:ident, $command:expr) => {
            #[test]
            fn $name() {
                assert!(matches!(gate_admin_market_command(true, Some(OTHER_GUILD),
                    Some(&market(1, GUILD, "open")), $command),
                    CommandGate::Rejected(reply) if reply.content.contains("not found in this server")));
            }
        };
    }

    cross_guild_case!(
        test_admin_market_commands_reject_cross_guild_market_cancel,
        AdminMarketCommand::Cancel
    );
    cross_guild_case!(
        test_admin_market_commands_reject_cross_guild_market_force_refresh,
        AdminMarketCommand::ForceRefresh
    );
    cross_guild_case!(
        test_admin_market_commands_reject_cross_guild_market_resolve,
        AdminMarketCommand::Resolve
    );
    cross_guild_case!(
        test_admin_market_commands_reject_cross_guild_market_set_fair,
        AdminMarketCommand::SetFair
    );
    cross_guild_case!(
        test_admin_market_commands_reject_cross_guild_market_view,
        AdminMarketCommand::View
    );

    #[test]
    fn test_resolve_announces_all_winners_and_losers() {
        let settlement = settlement(vec![
            participant(101, 2, 0, 11, 20),
            participant(102, 1, 0, 6, 10),
            participant(201, 0, 3, 16, 0),
            participant(202, 0, 1, 6, 0),
        ]);
        let lines = settlement_lines(&settlement);
        assert_eq!(lines.len(), 4);
        for id in [101, 102, 201, 202] {
            assert_eq!(
                lines
                    .iter()
                    .filter(|line| line.contains(&format!("<@{id}>")))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn test_resolve_names_market_in_announcement() {
        let chunks =
            resolution_announcement_chunks(7, "Will the chart ship?", ContractSide::Yes, &[]);
        assert!(chunks[0].contains("\"Will the chart ship?\""));
    }

    #[test]
    fn test_resolve_does_not_depend_on_a_post_settlement_market_read() {
        let captured_question = "Will one lookup be enough?".to_owned();
        let chunks = resolution_announcement_chunks(7, &captured_question, ContractSide::Yes, &[]);
        assert!(chunks[0].contains(&captured_question));
    }

    #[test]
    fn test_resolve_neutralizes_mentions_in_market_name() {
        let sanitized = resolve_and_neutralize_discord_mentions(
            "Will @everyone and @here back <@123> or <@&456>?",
            |id| (id == 123).then(|| "Player".to_owned()),
            |id| (id == 456).then(|| "Favorites".to_owned()),
        );
        assert_eq!(
            sanitized,
            "Will @\u{200b}everyone and @\u{200b}here back @Player or @Favorites?"
        );
    }

    #[test]
    fn test_resolve_attaches_chart_to_each_announcement_surface() {
        let sends = resolution_publish_plan(&["settled".to_owned()], true);
        assert_eq!(sends.len(), 3);
        assert!(sends.iter().all(|send| send.include_chart));
        assert_eq!(
            sends
                .iter()
                .map(|send| send.surface)
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn test_resolve_retries_complete_text_when_chart_uploads_fail() {
        let original =
            resolution_publish_plan(&["complete settlement".to_owned()], true)[0].clone();
        let retry =
            retry_without_attachment(&original, AttachmentResult::Forbidden).expect("retry");
        assert_eq!(retry.content, original.content);
        assert!(!retry.include_chart);
    }

    #[test]
    fn test_rollback_admin_only() {
        assert!(
            matches!(gate_admin_market_command(false, Some(GUILD), Some(&market(42, GUILD, "resolved")),
            AdminMarketCommand::Rollback), CommandGate::Rejected(reply)
            if reply.content == "Only admins can roll back markets." && reply.visibility == Visibility::Ephemeral)
        );
    }

    #[test]
    fn test_rollback_reopens_thread_refreshes_embed_and_announces() {
        let reply = rollback_reply(42, ContractSide::No, 30, 2);
        assert!(reply.content.contains("Market #42"));
        assert!(reply.content.contains("NO resolution rolled back"));
        let plan = refresh_embed_plan(
            ThreadState {
                archived: true,
                locked: true,
                message_exists: true,
            },
            true,
        );
        assert!(plan.contains(&ThreadAction::EditEmbed { restore_view: true }));
    }

    #[test]
    fn test_resolve_chunks_large_winner_and_loser_lists() {
        let lines = (0..160)
            .map(|index| {
                format!(
                    "<@{}> spent 900 (0 yes / 1 no) → won 0 JC (net -900)",
                    10_000 + index
                )
            })
            .collect::<Vec<_>>();
        let chunks = resolution_announcement_chunks(42, "Large market?", ContractSide::Yes, &lines);
        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.len() <= DISCORD_MESSAGE_LIMIT)
        );
        let all = chunks.join("\n");
        assert!(all.contains("<@10000>"));
        assert!(all.contains("<@10159>"));
    }

    #[test]
    fn test_resolve_rejects_cross_guild_market() {
        assert!(matches!(
            gate_admin_market_command(
                true,
                Some(OTHER_GUILD),
                Some(&market(42, GUILD, "open")),
                AdminMarketCommand::Resolve
            ),
            CommandGate::Rejected(_)
        ));
    }

    #[test]
    fn test_refresh_status_admin_only() {
        assert!(matches!(
            gate_admin_market_command(
                false,
                Some(GUILD),
                Some(&market(1, GUILD, "open")),
                AdminMarketCommand::ForceRefresh
            ),
            CommandGate::Rejected(_)
        ));
    }

    #[test]
    fn test_refresh_status_lists_only_this_guild() {
        let markets = vec![
            market(1, GUILD, "open"),
            market(2, GUILD, "open"),
            market(3, OTHER_GUILD, "open"),
            market(4, GUILD, "resolved"),
        ];
        assert_eq!(
            refresh_status_markets(&markets, Some(GUILD))
                .iter()
                .map(|market| market.prediction_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn test_refresh_market_embed_unarchives_thread_before_editing() {
        let plan = refresh_embed_plan(
            ThreadState {
                archived: true,
                locked: false,
                message_exists: true,
            },
            false,
        );
        assert_eq!(
            plan,
            vec![
                ThreadAction::FetchMessage,
                ThreadAction::Unarchive {
                    auto_archive_minutes: THREAD_AUTO_ARCHIVE_MINUTES
                },
                ThreadAction::EditEmbed {
                    restore_view: false
                }
            ]
        );
    }

    #[test]
    fn test_refresh_market_embed_leaves_live_thread_alone() {
        let plan = refresh_embed_plan(
            ThreadState {
                archived: false,
                locked: false,
                message_exists: true,
            },
            false,
        );
        assert_eq!(
            plan,
            vec![
                ThreadAction::FetchMessage,
                ThreadAction::EditEmbed {
                    restore_view: false
                }
            ]
        );
    }

    #[test]
    fn test_refresh_market_embed_restores_persistent_view() {
        let plan = refresh_embed_plan(
            ThreadState {
                archived: false,
                locked: false,
                message_exists: true,
            },
            true,
        );
        assert!(plan.contains(&ThreadAction::EditEmbed { restore_view: true }));
    }

    #[test]
    fn test_refresh_market_embed_vanished_message_does_not_revive_thread() {
        let plan = refresh_embed_plan(
            ThreadState {
                archived: true,
                locked: false,
                message_exists: false,
            },
            false,
        );
        assert_eq!(plan, vec![ThreadAction::FetchMessage]);
    }

    #[test]
    fn test_resolve_rewrites_embed_and_archives_even_if_announcement_fails() {
        let plan = resolved_thread_plan(ThreadState {
            archived: true,
            locked: false,
            message_exists: true,
        });
        assert!(plan.contains(&ThreadAction::EditEmbed {
            restore_view: false
        }));
        assert_eq!(plan.last(), Some(&ThreadAction::LockAndArchive));
    }

    #[test]
    fn test_create_uses_max_auto_archive_and_persists_thread_ids() {
        assert_eq!(THREAD_AUTO_ARCHIVE_MINUTES, 10_080);
        let mut setup = CreateSetupState::default();
        setup.complete(SetupBranch::Embed);
        setup.complete(SetupBranch::Pin);
        assert!(setup.can_persist());
        setup.complete(SetupBranch::Persist);
        assert!(setup.persisted);
    }

    #[test]
    fn test_create_overlaps_initial_branches_and_preserves_publish_order() {
        let mut setup = CreateSetupState::default();
        assert!(!setup.can_start_thread());
        setup.complete(SetupBranch::Announcement);
        assert!(setup.can_start_thread());
        setup.complete(SetupBranch::Thread);
        assert!(!setup.can_send_embed());
        setup.complete(SetupBranch::Chart);
        assert!(setup.can_send_embed());
        setup.complete(SetupBranch::Embed);
        setup.complete(SetupBranch::Pin);
        assert!(setup.can_persist());
    }

    #[test]
    fn test_create_confirmation_error_waits_for_setup_and_then_propagates() {
        let mut setup = CreateSetupState {
            confirmation_error: Some("confirmation failed".to_owned()),
            ..CreateSetupState::default()
        };
        setup.complete(SetupBranch::Confirmation);
        assert!(!setup.can_return());
        for branch in [
            SetupBranch::Announcement,
            SetupBranch::Chart,
            SetupBranch::Thread,
            SetupBranch::Embed,
            SetupBranch::Pin,
            SetupBranch::Persist,
        ] {
            setup.complete(branch);
        }
        assert!(setup.can_return());
        assert_eq!(
            setup.confirmation_error.as_deref(),
            Some("confirmation failed")
        );
    }

    #[test]
    fn test_create_optional_failure_waits_for_chart_and_does_not_persist_fake_ids() {
        let mut setup = CreateSetupState {
            announcement_error: Some("announcement failed".to_owned()),
            confirmation_done: true,
            ..CreateSetupState::default()
        };
        assert!(!setup.can_return());
        setup.complete(SetupBranch::Chart);
        assert!(setup.can_return());
        assert!(!setup.persisted);
    }

    macro_rules! quote_case {
        ($name:ident, $raw:expr, $buy:expr, $expected:expr) => {
            #[test]
            fn $name() {
                assert_eq!(quote_total($raw, $buy), $expected);
            }
        };
    }

    quote_case!(test_quote_total_rounding_200_buy_20, 200, true, 20);
    quote_case!(test_quote_total_rounding_200_sell_20, 200, false, 20);
    quote_case!(test_quote_total_rounding_201_buy_20, 201, true, 20);
    quote_case!(test_quote_total_rounding_201_sell_20, 201, false, 20);
    quote_case!(test_quote_total_rounding_205_buy_21, 205, true, 21);
    quote_case!(test_quote_total_rounding_205_sell_20, 205, false, 20);
    quote_case!(test_quote_total_rounding_206_buy_21, 206, true, 21);
    quote_case!(test_quote_total_rounding_206_sell_21, 206, false, 21);
    quote_case!(test_quote_total_rounding_4_buy_1, 4, true, 1);
    quote_case!(test_quote_total_rounding_4_sell_0, 4, false, 0);
    quote_case!(test_quote_total_rounding_0_buy_0, 0, true, 0);
    quote_case!(test_quote_total_rounding_0_sell_0, 0, false, 0);

    #[test]
    fn test_market_chart_renders_for_zero_one_and_many_snapshots() {
        let empty = chart_spec(1, 1_700_000_000, &[]).expect("empty chart");
        let one = chart_spec(2, 1_700_000_000, &[(1_700_000_060, 50)]).expect("one chart");
        let many = chart_spec(
            3,
            1_700_000_000,
            &[
                (1_700_000_000, 30),
                (1_700_003_600, 35),
                (1_700_007_200, 40),
            ],
        )
        .expect("many chart");
        assert!(!chart_fingerprint(&empty).is_empty());
        assert!(!chart_fingerprint(&one).is_empty());
        assert!(!chart_fingerprint(&many).is_empty());
    }

    #[test]
    fn test_market_chart_metadata_failure_returns_none() {
        assert!(chart_spec(1, -1, &[]).is_none());
        assert!(build_chart_attachment(None, true).is_none());
    }

    #[test]
    fn test_market_chart_file_construction_failure_returns_none() {
        let spec = chart_spec(1, 100, &[(101, 50)]).expect("chart");
        assert!(build_chart_attachment(Some(&spec), false).is_none());
    }

    #[test]
    fn test_market_chart_auto_zooms_for_narrow_band_markets() {
        let narrow = chart_spec(10, 100, &[(101, 17), (102, 19), (103, 22)]).expect("narrow");
        let wide = chart_spec(10, 100, &[(101, 5), (102, 50), (103, 95)]).expect("wide");
        assert_ne!((narrow.y_min, narrow.y_max), (wide.y_min, wide.y_max));
        assert_ne!(chart_fingerprint(&narrow), chart_fingerprint(&wide));
    }

    #[test]
    fn test_announce_to_gamba_forwards_file_when_present() {
        let chart = ChartAttachment {
            filename: "chart.png".to_owned(),
            bytes: vec![1, 2, 3],
        };
        let send = gamba_send("E", Some(chart.clone()));
        assert_eq!(send.file, Some(chart));
    }

    #[test]
    fn test_announce_to_gamba_omits_file_kwarg_when_none() {
        assert!(gamba_send("E", None).file.is_none());
    }

    #[test]
    fn test_chunk_message_lines_preserves_unicode_boundary() {
        let chunks = chunk_message_lines("header", &["🐑".repeat(3_000)], DISCORD_MESSAGE_LIMIT);
        assert_eq!(chunks.len(), 2);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.is_char_boundary(chunk.len()))
        );
    }
}
