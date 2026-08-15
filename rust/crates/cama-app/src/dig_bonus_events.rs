//! Transport-independent orchestration for rare bonuses after completed digs.
//!
//! Python remains the live Discord and schema authority. Random selection is
//! injected through the existing Dig loot entropy trait, trivia uses the
//! existing question type and Dig reward scaler, package writes reuse the
//! existing package-deal repository, and wallet settlement crosses one typed
//! existing-schema transaction port.

use cama_db::dig_bonus_events_repository::{
    BalanceAdjustmentRequest, DigBonusRepository, DigBonusRepositoryError,
};
use cama_db::package_deal_repository::{PackageDealRepository, PackageDealRepositoryError};
use cama_domain::dig_economy::{DIG_POSITIVE_JC_MULTIPLIER, scale_positive_dig_jc};
use cama_domain::embed_safety::EmbedModel;
use chrono::{Duration, Utc};
use thiserror::Error;

use crate::dig_loot::{EventComplexity, LootEntropy};
use crate::trivia_questions::TriviaQuestion;

pub const PACKAGE_ACTIVITY_DAYS: i64 = 14;
pub const PACKAGE_CANDIDATE_COUNT: usize = 4;
pub const FREE_PACKAGE_GAMES: i64 = 3;
pub const TRIVIA_CORRECT_GROSS_JC: i64 = 15;
pub const TRIVIA_WRONG_JC: i64 = -5;

pub const PARTIAL_BONUS_FAILURE: &str = "The bonus could not be displayed completely. Any JC change may already be recorded in the ledger.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigBonus {
    Wheel,
    PackageDeal,
    Trivia,
}

impl DigBonus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wheel => "wheel",
            Self::PackageDeal => "package_deal",
            Self::Trivia => "trivia",
        }
    }
}

/// Map one roll to mutually exclusive one-percent bands.
#[must_use]
pub fn pick_dig_bonus(roll: f64) -> Option<DigBonus> {
    if roll < 0.01 {
        Some(DigBonus::Wheel)
    } else if roll < 0.02 {
        Some(DigBonus::PackageDeal)
    } else if roll < 0.03 {
        Some(DigBonus::Trivia)
    } else {
        None
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuildMember {
    pub id: i64,
    pub display_name: String,
    pub bot: bool,
}

/// Deduplicate, exclude the digger, and sample four without replacement.
#[must_use]
pub fn choose_package_candidates(
    players: &[GuildMember],
    digger_id: i64,
    entropy: &mut impl LootEntropy,
) -> Vec<GuildMember> {
    let mut eligible = Vec::new();
    for player in players {
        if player.id != digger_id
            && !eligible
                .iter()
                .any(|candidate: &GuildMember| candidate.id == player.id)
        {
            eligible.push(player.clone());
        }
    }
    if eligible.len() < PACKAGE_CANDIDATE_COUNT {
        return Vec::new();
    }
    let mut selected = Vec::with_capacity(PACKAGE_CANDIDATE_COUNT);
    for _ in 0..PACKAGE_CANDIDATE_COUNT {
        let index = entropy.choose_index(eligible.len());
        selected.push(eligible.remove(index));
    }
    selected
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DigBonusPortError {
    #[error("dig bonus storage failed: {0}")]
    Backend(String),
}

impl From<DigBonusRepositoryError> for DigBonusPortError {
    fn from(value: DigBonusRepositoryError) -> Self {
        Self::Backend(value.to_string())
    }
}

impl From<PackageDealRepositoryError> for DigBonusPortError {
    fn from(value: PackageDealRepositoryError) -> Self {
        Self::Backend(value.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TriviaSettlementRequest<'a> {
    pub user_id: i64,
    pub guild_id: i64,
    pub delta: i64,
    pub category: &'a str,
    pub correct: bool,
    pub timed_out: bool,
}

impl TriviaSettlementRequest<'_> {
    #[must_use]
    pub fn metadata_json(self) -> String {
        if self.correct {
            format!(
                "{{\"correct\":true,\"timed_out\":{},\"gross_jc\":{TRIVIA_CORRECT_GROSS_JC},\"reward_multiplier\":{DIG_POSITIVE_JC_MULTIPLIER}}}",
                self.timed_out
            )
        } else {
            format!("{{\"correct\":false,\"timed_out\":{}}}", self.timed_out)
        }
    }

    #[must_use]
    pub const fn reason(self) -> &'static str {
        if self.correct {
            "dig bonus trivia correct answer"
        } else {
            "dig bonus trivia wrong answer"
        }
    }
}

pub trait DigBonusEconomyPort {
    fn recently_active_player_ids(
        &self,
        guild_id: i64,
        activity_days: i64,
    ) -> Result<Vec<i64>, DigBonusPortError>;

    fn settle_trivia(&self, request: TriviaSettlementRequest<'_>)
    -> Result<i64, DigBonusPortError>;
}

impl DigBonusEconomyPort for DigBonusRepository {
    fn recently_active_player_ids(
        &self,
        guild_id: i64,
        activity_days: i64,
    ) -> Result<Vec<i64>, DigBonusPortError> {
        let cutoff = Utc::now() - Duration::days(activity_days.max(0));
        DigBonusRepository::recently_active_player_ids(self, Some(guild_id), &cutoff.to_rfc3339())
            .map_err(Into::into)
    }

    fn settle_trivia(
        &self,
        request: TriviaSettlementRequest<'_>,
    ) -> Result<i64, DigBonusPortError> {
        let metadata = request.metadata_json();
        DigBonusRepository::adjust_balance_atomic(
            self,
            BalanceAdjustmentRequest {
                discord_id: request.user_id,
                guild_id: Some(request.guild_id),
                delta: request.delta,
                source: "dig",
                actor_id: request.user_id,
                related_type: "dig_bonus_trivia",
                related_id: request.category,
                reason: request.reason(),
                metadata_json: &metadata,
            },
        )
        .map(|adjustment| adjustment.balance_after)
        .map_err(Into::into)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FreePackageRequest {
    pub guild_id: i64,
    pub buyer_id: i64,
    pub partner_id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageActivation {
    pub games_added: i64,
    pub games_remaining: i64,
}

pub trait PackageDealBonusPort {
    fn create_free_package(
        &self,
        request: FreePackageRequest,
    ) -> Result<PackageActivation, DigBonusPortError>;
}

impl PackageDealBonusPort for PackageDealRepository {
    fn create_free_package(
        &self,
        request: FreePackageRequest,
    ) -> Result<PackageActivation, DigBonusPortError> {
        PackageDealRepository::create_or_extend_deal(
            self,
            Some(request.guild_id),
            request.buyer_id,
            request.partner_id,
            FREE_PACKAGE_GAMES,
            0,
        )
        .map(|deal| PackageActivation {
            games_added: deal.games_added,
            games_remaining: deal.games_remaining,
        })
        .map_err(Into::into)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MessageUpdate {
    pub content: Option<String>,
    pub embed: Option<EmbedModel>,
    pub clear_view: bool,
    pub clear_attachments: bool,
    pub ephemeral: bool,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum MessageEditError {
    #[error("message no longer exists")]
    NotFound,
    #[error("Discord rejected the message edit")]
    Http,
    #[error("message edit failed: {0}")]
    Other(String),
}

pub trait MessageEditor {
    fn edit(&mut self, update: &MessageUpdate) -> Result<(), MessageEditError>;
}

/// Discord `NotFound`/`HTTPException` are presentation failures after state may
/// already be committed, so callers deliberately suppress only those classes.
pub fn edit_message_best_effort(
    editor: &mut impl MessageEditor,
    update: &MessageUpdate,
) -> Result<(), MessageEditError> {
    match editor.edit(update) {
        Ok(()) | Err(MessageEditError::NotFound | MessageEditError::Http) => Ok(()),
        Err(error @ MessageEditError::Other(_)) => Err(error),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageDealSession {
    pub buyer_id: i64,
    pub guild_id: i64,
    pub candidates: Vec<GuildMember>,
    pub resolved: bool,
    pub choices_disabled: bool,
}

impl PackageDealSession {
    #[must_use]
    pub fn new(buyer_id: i64, guild_id: i64, candidates: Vec<GuildMember>) -> Self {
        Self {
            buyer_id,
            guild_id,
            candidates,
            resolved: false,
            choices_disabled: false,
        }
    }

    #[must_use]
    pub const fn interaction_allowed(&self, user_id: i64) -> bool {
        user_id == self.buyer_id
    }

    pub fn select_partner(
        &mut self,
        partner_id: i64,
        port: &impl PackageDealBonusPort,
    ) -> MessageUpdate {
        if self.resolved {
            return MessageUpdate {
                clear_view: true,
                ..MessageUpdate::default()
            };
        }
        self.resolved = true;
        self.choices_disabled = true;
        let candidate = self
            .candidates
            .iter()
            .find(|candidate| candidate.id == partner_id);
        let Some(candidate) = candidate else {
            return package_activation_failure();
        };
        match port.create_free_package(FreePackageRequest {
            guild_id: self.guild_id,
            buyer_id: self.buyer_id,
            partner_id,
        }) {
            Ok(deal) => MessageUpdate {
                content: Some(format!(
                    "The unearthed foreman's contract is signed with **{}**: **{} games added**, **{} games remaining**.",
                    candidate.display_name, deal.games_added, deal.games_remaining
                )),
                clear_view: true,
                ..MessageUpdate::default()
            },
            Err(_) => package_activation_failure(),
        }
    }

    pub fn on_timeout(&mut self) -> Option<MessageUpdate> {
        if self.resolved {
            return None;
        }
        self.choices_disabled = true;
        Some(MessageUpdate {
            content: Some(
                "*The unearthed foreman's contract crumbled to dust in the mine.*".to_owned(),
            ),
            clear_view: false,
            ..MessageUpdate::default()
        })
    }
}

fn package_activation_failure() -> MessageUpdate {
    MessageUpdate {
        content: Some(
            "The unearthed foreman's contract could not be activated. Try again later.".to_owned(),
        ),
        clear_view: true,
        ..MessageUpdate::default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigTriviaSession {
    pub user_id: i64,
    pub guild_id: i64,
    pub question: TriviaQuestion,
    pub correct_jc: i64,
    pub resolved: bool,
}

impl DigTriviaSession {
    #[must_use]
    pub fn new(user_id: i64, guild_id: i64, question: TriviaQuestion, correct_jc: i64) -> Self {
        Self {
            user_id,
            guild_id,
            question,
            correct_jc: correct_jc.max(0),
            resolved: false,
        }
    }

    #[must_use]
    pub const fn interaction_allowed(&self, user_id: i64) -> bool {
        user_id == self.user_id
    }

    pub fn answer(
        &mut self,
        choice_index: usize,
        port: &impl DigBonusEconomyPort,
    ) -> MessageUpdate {
        if self.resolved {
            return MessageUpdate {
                clear_view: true,
                ..MessageUpdate::default()
            };
        }
        self.resolved = true;
        let correct = choice_index == self.question.correct_index;
        self.settle(correct, false, port)
    }

    pub fn on_timeout(&mut self, port: &impl DigBonusEconomyPort) -> Option<MessageUpdate> {
        if self.resolved {
            return None;
        }
        self.resolved = true;
        Some(self.settle(false, true, port))
    }

    fn settle(
        &self,
        correct: bool,
        timed_out: bool,
        port: &impl DigBonusEconomyPort,
    ) -> MessageUpdate {
        let delta = if correct {
            self.correct_jc
        } else {
            TRIVIA_WRONG_JC
        };
        let request = TriviaSettlementRequest {
            user_id: self.user_id,
            guild_id: self.guild_id,
            delta,
            category: self.question.category,
            correct,
            timed_out,
        };
        let embed = match port.settle_trivia(request) {
            Ok(_) => trivia_result_embed(&self.question, correct, timed_out, delta),
            Err(_) => trivia_settlement_error_embed(),
        };
        MessageUpdate {
            embed: Some(embed),
            clear_view: true,
            clear_attachments: true,
            ..MessageUpdate::default()
        }
    }
}

#[must_use]
pub fn trivia_question_embed(question: &TriviaQuestion, correct_jc: i64) -> EmbedModel {
    let mut embed = EmbedModel {
        title: Some("⛏️ Unearthed in the Mine — Dota 2 Trivia".to_owned()),
        description: Some(format!(
            "A rune-covered stone tablet slides free from the mine wall. Decipher its challenge:\n\n{}",
            question.text
        )),
        footer: Some(format!(
            "+{} JC correct • -5 JC wrong or timeout",
            correct_jc.max(0)
        )),
        ..EmbedModel::default()
    };
    embed.add_field(
        "Choose one",
        question
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| format!("**{}.** {option}", ['A', 'B', 'C', 'D'][index]))
            .collect::<Vec<_>>()
            .join("\n"),
        false,
    );
    embed
}

fn trivia_result_embed(
    question: &TriviaQuestion,
    correct: bool,
    timed_out: bool,
    delta: i64,
) -> EmbedModel {
    let title = if timed_out {
        "⛏️ Unearthed Rune Tablet — Time's up! -5 JC".to_owned()
    } else if correct {
        format!("⛏️ Unearthed Rune Tablet — Correct! +{delta} JC")
    } else {
        "⛏️ Unearthed Rune Tablet — Wrong! -5 JC".to_owned()
    };
    let answer = question
        .options
        .get(question.correct_index)
        .map_or("", String::as_str);
    let mut description = format!("The rune tablet reveals the correct answer: **{answer}**.");
    if let Some(explanation) = question.explanation.as_deref() {
        description.push_str(&format!("\n\n*{explanation}*"));
    }
    EmbedModel {
        title: Some(title),
        description: Some(description),
        ..EmbedModel::default()
    }
}

fn trivia_settlement_error_embed() -> EmbedModel {
    EmbedModel {
        title: Some("⛏️ Unearthed Rune Tablet — Settlement Error".to_owned()),
        description: Some(
            "The mine's rune tablet flickers, but the JC result could not be confirmed. Please contact an admin to verify the balance ledger."
                .to_owned(),
        ),
        ..EmbedModel::default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageBonusPresentation {
    Offer {
        embed: EmbedModel,
        session: PackageDealSession,
    },
    InsufficientCandidates(MessageUpdate),
}

pub fn recently_active_package_pool(
    economy: &impl DigBonusEconomyPort,
    guild_id: i64,
) -> Result<Vec<i64>, DigBonusPortError> {
    economy.recently_active_player_ids(guild_id, PACKAGE_ACTIVITY_DAYS)
}

pub fn prepare_package_bonus(
    economy: &impl DigBonusEconomyPort,
    guild_id: i64,
    digger_id: i64,
    members: &[GuildMember],
    entropy: &mut impl LootEntropy,
) -> Result<PackageBonusPresentation, DigBonusPortError> {
    let active_ids = recently_active_package_pool(economy, guild_id)?;
    let active_members = active_ids
        .into_iter()
        .filter_map(|id| members.iter().find(|member| member.id == id))
        .filter(|member| !member.bot)
        .cloned()
        .collect::<Vec<_>>();
    let candidates = choose_package_candidates(&active_members, digger_id, entropy);
    if candidates.is_empty() {
        return Ok(PackageBonusPresentation::InsufficientCandidates(
            MessageUpdate {
                content: Some(
                    "Your pickaxe unearthed a foreman's Package Deal contract, but there were not four eligible active players to sign it."
                        .to_owned(),
                ),
                ephemeral: true,
                ..MessageUpdate::default()
            },
        ));
    }
    Ok(PackageBonusPresentation::Offer {
        embed: EmbedModel {
            title: Some("⛏️ Unearthed in the Mine — Package Deal".to_owned()),
            description: Some(
                "Your pickaxe cracks open a sealed foreman's contract buried in the rock. Choose one active player to share a free Package Deal for **3 games**."
                    .to_owned(),
            ),
            ..EmbedModel::default()
        },
        session: PackageDealSession::new(digger_id, guild_id, candidates),
    })
}

#[must_use]
pub fn prepare_trivia_bonus(
    user_id: i64,
    guild_id: i64,
    question: TriviaQuestion,
    event_adjusted_gross_jc: i64,
) -> (EmbedModel, DigTriviaSession) {
    let correct_jc = scale_positive_dig_jc(event_adjusted_gross_jc).max(0);
    let embed = trivia_question_embed(&question, correct_jc);
    let session = DigTriviaSession::new(user_id, guild_id, question, correct_jc);
    (embed, session)
}

pub trait WheelBonusPort {
    fn trigger_bonus_spin(&mut self) -> Result<(), DigBonusPortError>;
}

pub fn send_wheel_bonus(port: &mut impl WheelBonusPort) -> Result<(), DigBonusPortError> {
    port.trigger_bonus_spin()
}

pub trait BonusDispatchPort {
    fn send_bonus(&mut self, bonus: DigBonus) -> Result<(), DigBonusPortError>;
    fn report_partial_failure(&mut self, content: &str) -> Result<(), DigBonusPortError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BonusDispatchOutcome {
    NotConsumed,
    NoBonus,
    Sent(DigBonus),
    FailedButDigPreserved(DigBonus),
}

/// Bonus failures are isolated after the Dig transaction has already
/// succeeded. Even failure reporting is best-effort and cannot mask the dig.
pub fn maybe_send_dig_bonus(
    dig_consumed: bool,
    roll: f64,
    port: &mut impl BonusDispatchPort,
) -> BonusDispatchOutcome {
    if !dig_consumed {
        return BonusDispatchOutcome::NotConsumed;
    }
    let Some(bonus) = pick_dig_bonus(roll) else {
        return BonusDispatchOutcome::NoBonus;
    };
    match port.send_bonus(bonus) {
        Ok(()) => BonusDispatchOutcome::Sent(bonus),
        Err(_) => {
            let _ = port.report_partial_failure(PARTIAL_BONUS_FAILURE);
            BonusDispatchOutcome::FailedButDigPreserved(bonus)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigDispatchResult {
    pub boss_encounter: bool,
    pub event_complexity: Option<EventComplexity>,
    pub dig_consumed: bool,
}

pub trait DigResultDispatchPort {
    type Error;

    fn send_normal(&mut self) -> Result<(), Self::Error>;
    fn send_boss(&mut self) -> Result<(), Self::Error>;
    fn send_boon(&mut self) -> Result<(), Self::Error>;
    fn send_choice(&mut self) -> Result<(), Self::Error>;
    fn send_bonus_after_ui(&mut self, dig_consumed: bool) -> Result<(), Self::Error>;
}

pub fn dispatch_dig_result<P: DigResultDispatchPort>(
    result: DigDispatchResult,
    port: &mut P,
) -> Result<(), P::Error> {
    if result.boss_encounter {
        port.send_boss()?;
    } else {
        match result.event_complexity {
            Some(EventComplexity::Boon) => port.send_boon()?,
            Some(EventComplexity::Choice | EventComplexity::Complex) => port.send_choice()?,
            Some(EventComplexity::Simple) | None => port.send_normal()?,
        }
    }
    port.send_bonus_after_ui(result.dig_consumed)
}

pub fn dispatch_paid_dig_bonus<P: DigResultDispatchPort>(
    paid_result_consumed: bool,
    port: &mut P,
) -> Result<(), P::Error> {
    port.send_bonus_after_ui(paid_result_consumed)
}

#[cfg(test)]
mod tests;
