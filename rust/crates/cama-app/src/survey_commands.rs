//! Survey models, embed/component rendering, recipient resolution, and
//! delivery-receipt reconciliation used by the runtime survey provider.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

pub const DELIVERY_BATCH_SIZE: usize = 25;
pub const DELIVERY_CONCURRENCY: usize = 5;
pub const DELIVERY_STALE_SECONDS: i64 = 300;
pub const DELIVERY_RECEIPT_GRACE_SECONDS: i64 = 30;
pub const DELIVERY_RECEIPT_SCAN_LIMIT: usize = 500;
pub const RESULTS_TIMEOUT_SECONDS: i64 = 600;
pub const PREVIEW_QUESTIONS_PER_PAGE: usize = 5;
pub const EMBED_CONTENT_BUDGET: usize = 5_900;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurveyStatus {
    Draft,
    Open,
    Closed,
}

impl SurveyStatus {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Open => "Open",
            Self::Closed => "Closed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurveyTargetType {
    AllRegistered,
    Member,
    Role,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuestionType {
    Nps,
    Rating,
    Text,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryStatus {
    Pending,
    Sending,
    Sent,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Survey {
    pub survey_id: i64,
    pub guild_id: i64,
    pub title: String,
    pub status: SurveyStatus,
    pub target_type: Option<SurveyTargetType>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Question {
    pub question_id: i64,
    pub position: usize,
    pub prompt: String,
    pub question_type: QuestionType,
    pub required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct DraftAnswer {
    pub question_id: i64,
    pub numeric_value: Option<i32>,
    pub text_value: Option<String>,
    pub skipped: bool,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recipient {
    pub recipient_id: i64,
    pub survey_id: i64,
    pub guild_id: i64,
    pub discord_id: i64,
    pub delivery_status: DeliveryStatus,
    pub attempt_count: u32,
    pub claimed_at: Option<i64>,
    pub dm_channel_id: Option<i64>,
    pub dm_message_id: Option<i64>,
    pub ui_message_id: Option<i64>,
    pub current_question_id: Option<i64>,
    pub in_review: bool,
    pub submitted_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Session {
    pub survey: Survey,
    pub recipient: Recipient,
    pub questions: Vec<Question>,
    pub answers: Vec<DraftAnswer>,
}

impl Session {
    #[must_use]
    pub fn current_question(&self) -> Option<&Question> {
        let id = self.recipient.current_question_id?;
        self.questions
            .iter()
            .find(|question| question.question_id == id)
    }

    #[must_use]
    pub fn message_id(&self) -> Option<i64> {
        self.recipient
            .ui_message_id
            .or(self.recipient.dm_message_id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuestionResult {
    pub question_id: i64,
    pub position: usize,
    pub prompt: String,
    pub question_type: QuestionType,
    pub response_count: usize,
    pub skipped_count: usize,
    pub nps_score: Option<i32>,
    pub promoters: usize,
    pub passives: usize,
    pub detractors: usize,
    pub rating_average: Option<f64>,
    pub rating_distribution: [usize; 5],
    pub text_responses: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurveyResults {
    pub survey_id: i64,
    pub title: String,
    pub status: SurveyStatus,
    pub recipient_count: usize,
    pub delivered_count: usize,
    pub started_count: usize,
    pub completed_count: usize,
    pub delivery_rate: f64,
    pub started_rate: f64,
    pub completion_rate: f64,
    pub response_rate: f64,
    pub questions: Vec<QuestionResult>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Embed {
    pub title: String,
    pub description: String,
    pub fields: Vec<EmbedField>,
    pub footer: Option<String>,
}

impl Embed {
    #[must_use]
    pub fn content_len(&self) -> usize {
        self.title.len()
            + self.description.len()
            + self
                .fields
                .iter()
                .map(|field| field.name.len() + field.value.len())
                .sum::<usize>()
            + self.footer.as_ref().map_or(0, String::len)
    }

    #[must_use]
    pub fn discord_safe(&self) -> bool {
        self.title.len() <= 256
            && self.description.len() <= 4_096
            && self.fields.len() <= 25
            && self.fields.iter().all(|field| {
                field.name.len() <= 256 && !field.name.is_empty() && field.value.len() <= 1_024
            })
            && self
                .footer
                .as_ref()
                .is_none_or(|footer| footer.len() <= 2_048)
            && self.content_len() <= 6_000
    }
}

pub use cama_domain::formatting::escape_discord_text;

fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let suffix = "…";
    let mut out = String::new();
    for character in value.chars() {
        if out.len() + character.len_utf8() + suffix.len() > limit {
            break;
        }
        out.push(character);
    }
    out.push_str(suffix);
    out
}

#[must_use]
pub fn escaped_field_chunks(value: &str, limit: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        let escaped = escape_discord_text(&character.to_string());
        if !current.is_empty() && current.len() + escaped.len() > limit {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(&escaped);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push("\u{200b}".to_owned());
    }
    chunks
}

#[must_use]
pub fn question_embed(session: &Session, question: &Question) -> Embed {
    let number = session
        .questions
        .iter()
        .position(|candidate| candidate.question_id == question.question_id)
        .map_or(1, |position| position + 1);
    let how = match question.question_type {
        QuestionType::Nps => "Choose a score from 0 to 10.",
        QuestionType::Rating => "Choose a rating from 1 to 5.",
        QuestionType::Text => "Write your response in the private text form.",
    };
    let privacy = if session.survey.target_type == Some(SurveyTargetType::Member) {
        "This survey was sent only to you, so your answers are shared with the survey admins and are not anonymous."
    } else {
        "Admins receive anonymous, per-question results. Your answers are not shown with your Discord identity."
    };
    let requirement = if question.required {
        "Required"
    } else {
        "Optional"
    };
    let edit = if session.recipient.in_review {
        " • Editing from review"
    } else {
        ""
    };
    Embed {
        title: format!("Survey — {}", escape_discord_text(&session.survey.title)),
        description: escape_discord_text(&question.prompt),
        fields: vec![
            EmbedField {
                name: "How to answer".to_owned(),
                value: how.to_owned(),
                inline: false,
            },
            EmbedField {
                name: "Privacy".to_owned(),
                value: privacy.to_owned(),
                inline: false,
            },
        ],
        footer: Some(format!(
            "Question {number}/{} • {requirement}{edit}",
            session.questions.len()
        )),
    }
}

#[must_use]
pub fn review_embed(session: &Session) -> Embed {
    let answers: BTreeMap<_, _> = session
        .answers
        .iter()
        .map(|answer| (answer.question_id, answer))
        .collect();
    let fields = session
        .questions
        .iter()
        .map(|question| {
            let rendered = match answers.get(&question.question_id) {
                None => "Not answered".to_owned(),
                Some(answer) if answer.skipped => "Skipped".to_owned(),
                Some(answer) if answer.text_value.is_some() => {
                    escape_discord_text(answer.text_value.as_deref().unwrap_or_default())
                }
                Some(answer) => answer
                    .numeric_value
                    .map_or_else(String::new, |v| v.to_string()),
            };
            EmbedField {
                name: truncate(
                    &format!(
                        "{}. {}",
                        question.position,
                        escape_discord_text(&question.prompt)
                    ),
                    150,
                ),
                value: truncate(&rendered, 350),
                inline: false,
            }
        })
        .collect();
    Embed {
        title: format!("Review — {}", escape_discord_text(&session.survey.title)),
        description: "Review your answers, edit any question you want, then submit. Nothing appears in admin results until you submit.".to_owned(),
        fields,
        footer: Some("Your response has not been submitted yet.".to_owned()),
    }
}

#[must_use]
pub fn submitted_embed(session: &Session) -> Embed {
    let saved = if session.survey.target_type == Some(SurveyTargetType::Member) {
        "Your response was saved."
    } else {
        "Your response was saved anonymously."
    };
    Embed {
        title: "Survey submitted".to_owned(),
        description: format!(
            "Thanks for completing **{}**. {saved}",
            escape_discord_text(&session.survey.title)
        ),
        fields: Vec::new(),
        footer: None,
    }
}

#[must_use]
pub fn closed_embed(session: &Session) -> Embed {
    Embed {
        title: "Survey closed".to_owned(),
        description: format!(
            "**{}** is no longer accepting responses.",
            escape_discord_text(&session.survey.title)
        ),
        fields: Vec::new(),
        footer: None,
    }
}

#[must_use]
pub fn preview_pages(survey: &Survey, questions: &[Question]) -> Vec<Embed> {
    if questions.is_empty() {
        return vec![Embed {
            title: format!(
                "#{} — {}",
                survey.survey_id,
                escape_discord_text(&survey.title)
            ),
            description: format!("Status: **{}**", survey.status.label()),
            fields: vec![EmbedField {
                name: "Questions".to_owned(),
                value: "No questions yet.".to_owned(),
                inline: false,
            }],
            footer: None,
        }];
    }
    let mut pages: Vec<Embed> = Vec::new();
    let mut current: Option<Embed> = None;
    let mut questions_on_page = 0;
    for question in questions {
        let requirement = if question.required {
            "required"
        } else {
            "optional"
        };
        let field_name = format!(
            "{}. {:?} ({requirement}) • ID {}",
            question.position, question.question_type, question.question_id
        )
        .to_uppercase();
        let chunks = escaped_field_chunks(&question.prompt, 1_024);
        let field_size = field_name.len() + chunks.iter().map(String::len).sum::<usize>();
        let should_split = current.as_ref().is_none_or(|page| {
            questions_on_page >= PREVIEW_QUESTIONS_PER_PAGE
                || page.content_len() + field_size > EMBED_CONTENT_BUDGET
                || page.fields.len() + chunks.len() > 25
        });
        if should_split {
            if let Some(page) = current.take() {
                pages.push(page);
            }
            current = Some(Embed {
                title: format!(
                    "#{} — {}",
                    survey.survey_id,
                    escape_discord_text(&survey.title)
                ),
                description: format!("Status: **{}**", survey.status.label()),
                fields: Vec::new(),
                footer: None,
            });
            questions_on_page = 0;
        }
        if let Some(page) = current.as_mut() {
            for (index, chunk) in chunks.into_iter().enumerate() {
                page.fields.push(EmbedField {
                    name: if index == 0 {
                        field_name.clone()
                    } else {
                        "\u{200b}".to_owned()
                    },
                    value: chunk,
                    inline: false,
                });
            }
        }
        questions_on_page += 1;
    }
    if let Some(page) = current {
        pages.push(page);
    }
    let total = pages.len();
    if total > 1 {
        for (index, page) in pages.iter_mut().enumerate() {
            page.footer = Some(format!("Page {}/{total}", index + 1));
        }
    }
    pages
}

fn percent(value: f64) -> String {
    format!("{:.1}%", value * 100.0)
}

#[must_use]
pub fn results_pages(results: &SurveyResults) -> Vec<Embed> {
    let mut pages = vec![Embed {
        title: format!("Survey results — {}", escape_discord_text(&results.title)),
        description: String::new(),
        fields: vec![
            EmbedField {
                name: "Status".to_owned(),
                value: results.status.label().to_owned(),
                inline: true,
            },
            EmbedField {
                name: "Delivery".to_owned(),
                value: format!(
                    "{}/{} ({})",
                    results.delivered_count,
                    results.recipient_count,
                    percent(results.delivery_rate)
                ),
                inline: true,
            },
            EmbedField {
                name: "Completed".to_owned(),
                value: format!(
                    "{}/{} delivered ({})",
                    results.completed_count,
                    results.delivered_count,
                    percent(results.response_rate)
                ),
                inline: true,
            },
            EmbedField {
                name: "Started".to_owned(),
                value: format!(
                    "{}/{} delivered ({})",
                    results.started_count,
                    results.delivered_count,
                    percent(results.started_rate)
                ),
                inline: true,
            },
            EmbedField {
                name: "Completion after starting".to_owned(),
                value: percent(results.completion_rate),
                inline: true,
            },
        ],
        footer: Some("Only submitted responses are included below.".to_owned()),
    }];
    for question in &results.questions {
        let prompt = escape_discord_text(&question.prompt);
        match question.question_type {
            QuestionType::Nps => pages.push(Embed {
                title: format!("Question {} — NPS", question.position),
                description: prompt,
                fields: vec![
                    EmbedField {
                        name: "NPS".to_owned(),
                        value: question
                            .nps_score
                            .map_or_else(|| "N/A".to_owned(), |value| value.to_string()),
                        inline: true,
                    },
                    EmbedField {
                        name: "Answered / skipped".to_owned(),
                        value: format!("{} / {}", question.response_count, question.skipped_count),
                        inline: true,
                    },
                    EmbedField {
                        name: "Promoters (9–10)".to_owned(),
                        value: question.promoters.to_string(),
                        inline: true,
                    },
                    EmbedField {
                        name: "Passives (7–8)".to_owned(),
                        value: question.passives.to_string(),
                        inline: true,
                    },
                    EmbedField {
                        name: "Detractors (0–6)".to_owned(),
                        value: question.detractors.to_string(),
                        inline: true,
                    },
                ],
                footer: None,
            }),
            QuestionType::Rating => pages.push(Embed {
                title: format!("Question {} — Rating", question.position),
                description: prompt,
                fields: vec![
                    EmbedField {
                        name: "Average".to_owned(),
                        value: question
                            .rating_average
                            .map_or_else(|| "N/A".to_owned(), |value| format!("{value:.2}")),
                        inline: true,
                    },
                    EmbedField {
                        name: "Answered / skipped".to_owned(),
                        value: format!("{} / {}", question.response_count, question.skipped_count),
                        inline: true,
                    },
                    EmbedField {
                        name: "Distribution".to_owned(),
                        value: question
                            .rating_distribution
                            .iter()
                            .enumerate()
                            .map(|(index, count)| format!("{}: {count}", index + 1))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        inline: false,
                    },
                ],
                footer: None,
            }),
            QuestionType::Text => {
                if question.text_responses.is_empty() {
                    pages.push(Embed {
                        title: format!("Question {} — Written responses", question.position),
                        description: prompt,
                        fields: vec![EmbedField {
                            name: "Responses".to_owned(),
                            value: format!(
                                "No written responses • {} skipped",
                                question.skipped_count
                            ),
                            inline: false,
                        }],
                        footer: None,
                    });
                } else {
                    // Responses can each be up to 1000 characters and escaping
                    // roughly doubles them, so a fixed four-per-page split can
                    // exceed Discord's embed budget and fail the whole
                    // followup. Fill pages by size, as preview_pages does.
                    let title = format!("Question {} — Written responses", question.position);
                    let footer = format!(
                        "{} answered • {} skipped",
                        question.response_count, question.skipped_count
                    );
                    let mut fields: Vec<EmbedField> = Vec::new();
                    let flush = |fields: &mut Vec<EmbedField>| {
                        if fields.is_empty() {
                            return None;
                        }
                        Some(Embed {
                            title: title.clone(),
                            description: prompt.clone(),
                            fields: std::mem::take(fields),
                            footer: Some(footer.clone()),
                        })
                    };
                    for (response_index, response) in question.text_responses.iter().enumerate() {
                        let response_number = response_index + 1;
                        let chunks = escaped_field_chunks(response, 1_024);
                        let names = chunks
                            .iter()
                            .enumerate()
                            .map(|(index, _)| {
                                if index == 0 {
                                    format!("Anonymous response {response_number}")
                                } else {
                                    "\u{200b}".to_owned()
                                }
                            })
                            .collect::<Vec<_>>();
                        let size = names.iter().map(String::len).sum::<usize>()
                            + chunks.iter().map(String::len).sum::<usize>();
                        let used = title.len()
                            + prompt.len()
                            + footer.len()
                            + fields
                                .iter()
                                .map(|field| field.name.len() + field.value.len())
                                .sum::<usize>();
                        if !fields.is_empty()
                            && (used + size > EMBED_CONTENT_BUDGET
                                || fields.len() + chunks.len() > 25)
                            && let Some(page) = flush(&mut fields)
                        {
                            pages.push(page);
                        }
                        for (name, value) in names.into_iter().zip(chunks) {
                            fields.push(EmbedField {
                                name,
                                value,
                                inline: false,
                            });
                        }
                    }
                    if let Some(page) = flush(&mut fields) {
                        pages.push(page);
                    }
                }
            }
        }
    }
    pages
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberSnapshot {
    pub discord_id: i64,
    pub guild_id: i64,
    pub bot: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuildSnapshot {
    pub guild_id: i64,
    pub members: Vec<MemberSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoleSnapshot {
    pub guild_id: i64,
    pub members: Vec<MemberSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecipientTarget<'a> {
    AllRegistered,
    Member(&'a MemberSnapshot),
    Role(&'a RoleSnapshot),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipientError(pub &'static str);

pub fn resolve_recipients(
    guild: &GuildSnapshot,
    target: RecipientTarget<'_>,
    registered_ids: &[i64],
    registered_only: bool,
) -> Result<Vec<i64>, RecipientError> {
    let current: BTreeMap<_, _> = guild
        .members
        .iter()
        .map(|member| (member.discord_id, member))
        .collect();
    let registered: BTreeSet<_> = registered_ids
        .iter()
        .copied()
        .filter(|id| *id > 0)
        .collect();
    let result = match target {
        RecipientTarget::AllRegistered => {
            if registered_only {
                return Err(RecipientError(
                    "All registered targeting is already registered-only",
                ));
            }
            current
                .values()
                .filter(|member| !member.bot && registered.contains(&member.discord_id))
                .map(|member| member.discord_id)
                .collect()
        }
        RecipientTarget::Member(member) => {
            let current_member = current.get(&member.discord_id);
            if member.bot || current_member.is_some_and(|candidate| candidate.bot) {
                return Err(RecipientError("Bots cannot receive surveys"));
            }
            if member.guild_id != guild.guild_id || current_member.is_none() {
                return Err(RecipientError("That member is not in this server"));
            }
            if registered_only && !registered.contains(&member.discord_id) {
                return Err(RecipientError(
                    "That member is not a registered Cama player",
                ));
            }
            vec![member.discord_id]
        }
        RecipientTarget::Role(role) => {
            if role.guild_id != guild.guild_id {
                return Err(RecipientError("That role is not in this server"));
            }
            role.members
                .iter()
                .filter(|member| {
                    current
                        .get(&member.discord_id)
                        .is_some_and(|current_member| !member.bot && !current_member.bot)
                        && (!registered_only || registered.contains(&member.discord_id))
                })
                .map(|member| member.discord_id)
                .collect()
        }
    };
    Ok(result)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageComponent {
    pub custom_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryMessage {
    pub message_id: i64,
    pub author_id: i64,
    pub nonce: Option<String>,
    pub components: Vec<MessageComponent>,
}

#[must_use]
pub fn delivery_nonce(recipient_id: i64) -> String {
    format!("cama-s:{recipient_id:x}")
}

#[must_use]
pub fn message_matches_delivery(message: &HistoryMessage, survey_id: i64) -> bool {
    let prefix = format!("survey:{survey_id}:");
    message
        .components
        .iter()
        .filter_map(|component| component.custom_id.as_deref())
        .any(|custom_id| custom_id.starts_with(&prefix))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Control {
    Score {
        values: Vec<i32>,
        custom_id: String,
    },
    Text {
        custom_id: String,
    },
    Skip {
        custom_id: String,
    },
    ReturnToReview {
        custom_id: String,
    },
    Edit {
        options: Vec<ReviewOption>,
        custom_id: String,
    },
    Submit {
        custom_id: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewOption {
    pub value: i64,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResponseView {
    pub owner_id: i64,
    pub timeout_seconds: Option<i64>,
    pub controls: Vec<Control>,
}

impl ResponseView {
    #[must_use]
    pub fn interaction_check(&self, discord_id: i64) -> bool {
        self.owner_id == discord_id
    }
}

#[must_use]
pub fn question_view(session: &Session, question: &Question) -> ResponseView {
    let prefix = format!(
        "survey:{}:question:{}",
        session.survey.survey_id, question.question_id
    );
    let mut controls = vec![match question.question_type {
        QuestionType::Nps => Control::Score {
            values: (0..=10).collect(),
            custom_id: format!("{prefix}:score"),
        },
        QuestionType::Rating => Control::Score {
            values: (1..=5).collect(),
            custom_id: format!("{prefix}:score"),
        },
        QuestionType::Text => Control::Text {
            custom_id: format!("{prefix}:text"),
        },
    }];
    if !question.required {
        controls.push(Control::Skip {
            custom_id: format!("{prefix}:skip"),
        });
    }
    if session.recipient.in_review {
        controls.push(Control::ReturnToReview {
            custom_id: format!("survey:{}:review:return", session.survey.survey_id),
        });
    }
    ResponseView {
        owner_id: session.recipient.discord_id,
        timeout_seconds: None,
        controls,
    }
}

#[must_use]
pub fn review_view(session: &Session) -> ResponseView {
    ResponseView {
        owner_id: session.recipient.discord_id,
        timeout_seconds: None,
        controls: vec![
            Control::Edit {
                options: session
                    .questions
                    .iter()
                    .map(|question| ReviewOption {
                        value: question.question_id,
                        description: truncate(
                            &escape_discord_text(&question.prompt.replace('\n', " ")),
                            90,
                        ),
                    })
                    .collect(),
                custom_id: format!("survey:{}:review:edit", session.survey.survey_id),
            },
            Control::Submit {
                custom_id: format!("survey:{}:review:submit", session.survey.survey_id),
            },
        ],
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenderedResponse {
    Question { embed: Embed, view: ResponseView },
    Review { embed: Embed, view: ResponseView },
    Submitted { embed: Embed },
    Closed { embed: Embed },
}

#[must_use]
pub fn render_response(session: &Session) -> Option<RenderedResponse> {
    if session.survey.status == SurveyStatus::Closed {
        return Some(RenderedResponse::Closed {
            embed: closed_embed(session),
        });
    }
    if session.recipient.submitted_at.is_some() {
        return Some(RenderedResponse::Submitted {
            embed: submitted_embed(session),
        });
    }
    if session.recipient.in_review && session.recipient.current_question_id.is_none() {
        return Some(RenderedResponse::Review {
            embed: review_embed(session),
            view: review_view(session),
        });
    }
    let question = session.current_question()?;
    Some(RenderedResponse::Question {
        embed: question_embed(session, question),
        view: question_view(session, question),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiptSnapshot {
    pub recipient_id: i64,
    pub attempt_count: u32,
    pub claimed_at: Option<i64>,
    pub delivery_status: DeliveryStatus,
    pub updated_at: i64,
}

impl From<&Recipient> for ReceiptSnapshot {
    fn from(value: &Recipient) -> Self {
        Self {
            recipient_id: value.recipient_id,
            attempt_count: value.attempt_count,
            claimed_at: value.claimed_at,
            delivery_status: value.delivery_status,
            updated_at: value.updated_at,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SentMessage {
    pub channel_id: i64,
    pub message_id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReceiptReconciliation {
    Found(SentMessage),
    Checked(ReceiptSnapshot),
}

#[must_use]
pub fn reconcile_delivery_receipt(
    recipient: &Recipient,
    bot_user_id: i64,
    channel_id: i64,
    history: &[HistoryMessage],
) -> ReceiptReconciliation {
    history
        .iter()
        .take(DELIVERY_RECEIPT_SCAN_LIMIT)
        .find(|message| {
            message.author_id == bot_user_id
                && message_matches_delivery(message, recipient.survey_id)
        })
        .map_or_else(
            || ReceiptReconciliation::Checked(ReceiptSnapshot::from(recipient)),
            |message| {
                ReceiptReconciliation::Found(SentMessage {
                    channel_id,
                    message_id: message.message_id,
                })
            },
        )
}

#[must_use]
pub fn response_state_fingerprint(session: &Session, message_id: i64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    message_id.hash(&mut hasher);
    session.survey.status.label().hash(&mut hasher);
    session.recipient.updated_at.hash(&mut hasher);
    session.recipient.current_question_id.hash(&mut hasher);
    session.recipient.in_review.hash(&mut hasher);
    session.recipient.submitted_at.hash(&mut hasher);
    session.answers.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
#[path = "survey_commands/tests.rs"]
mod tests;
