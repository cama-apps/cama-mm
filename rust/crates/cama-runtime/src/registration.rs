use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;

pub const DISCORD_GLOBAL_COMMAND_LIMIT: usize = 100;
pub const DISCORD_COMMAND_OPTION_LIMIT: usize = 25;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOptionKind {
    Subcommand,
    SubcommandGroup,
    String,
    Integer,
    Boolean,
    User,
    Channel,
    Role,
    Mentionable,
    Number,
    Attachment,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CommandOptionChoice {
    String { name: String, value: String },
    Integer { name: String, value: i64 },
    Number { name: String, value: f64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommandOptionSpec {
    pub name: String,
    pub description: String,
    pub kind: CommandOptionKind,
    pub required: bool,
    pub options: Vec<Self>,
    pub choices: Vec<CommandOptionChoice>,
    pub min_integer: Option<i64>,
    pub max_integer: Option<i64>,
    pub min_number: Option<f64>,
    pub max_number: Option<f64>,
    pub min_length: Option<u16>,
    pub max_length: Option<u16>,
    pub autocomplete: bool,
}

impl CommandOptionSpec {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        kind: CommandOptionKind,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            kind,
            required: false,
            options: Vec::new(),
            choices: Vec::new(),
            min_integer: None,
            max_integer: None,
            min_number: None,
            max_number: None,
            min_length: None,
            max_length: None,
            autocomplete: false,
        }
    }

    #[must_use]
    pub const fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    #[must_use]
    pub fn options(mut self, options: Vec<Self>) -> Self {
        self.options = options;
        self
    }

    #[must_use]
    pub const fn autocomplete(mut self) -> Self {
        self.autocomplete = true;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum InteractionValue {
    Subcommand(Vec<InteractionOption>),
    SubcommandGroup(Vec<InteractionOption>),
    String(String),
    Integer(i64),
    Boolean(bool),
    User {
        id: u64,
        display_name: Option<String>,
        is_bot: Option<bool>,
    },
    Channel(u64),
    Role(u64),
    Mentionable(u64),
    Number(f64),
    Attachment {
        id: u64,
        filename: String,
        url: String,
        size: u64,
    },
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InteractionOption {
    pub name: String,
    pub value: InteractionValue,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InteractionRequest {
    Command {
        interaction_id: u64,
        name: String,
        user_id: u64,
        user_display_name: String,
        guild_id: Option<u64>,
        channel_id: Option<u64>,
        member_permissions: Option<u64>,
        options: Vec<InteractionOption>,
    },
    Autocomplete {
        interaction_id: u64,
        name: String,
        user_id: u64,
        guild_id: Option<u64>,
        channel_id: Option<u64>,
        focused_option: String,
        focused_value: String,
        options: Vec<InteractionOption>,
    },
    Component {
        interaction_id: u64,
        custom_id: String,
        user_id: u64,
        user_display_name: String,
        guild_id: Option<u64>,
        channel_id: Option<u64>,
        member_permissions: Option<u64>,
        values: Vec<String>,
    },
    Modal {
        interaction_id: u64,
        custom_id: String,
        user_id: u64,
        guild_id: Option<u64>,
        channel_id: Option<u64>,
        member_permissions: Option<u64>,
        fields: BTreeMap<String, String>,
    },
}

/// Controls whether the Discord transport may acknowledge an interaction
/// while its handler is still running.
///
/// Most component and modal-submit handlers use an automatic deferred update,
/// matching discord.py's `safe_defer(..., thinking=False)` pattern. Buttons
/// whose initial response must be a modal opt out because Discord rejects a
/// modal after any other acknowledgement.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InteractionAcknowledgementPolicy {
    #[default]
    Automatic,
    Modal,
}

/// State of the one allowed initial Discord interaction response.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InteractionInitialResponseState {
    #[default]
    NotStarted,
    Attempting,
    Accepted,
    Rejected,
}

impl InteractionRequest {
    #[must_use]
    pub fn root_name(&self) -> &str {
        match self {
            Self::Command { name, .. } | Self::Autocomplete { name, .. } => name,
            Self::Component { .. } | Self::Modal { .. } => "",
        }
    }

    /// Resolve Discord's qualified slash-command name from the nested
    /// subcommand/group payload (`admin health`, `survey send`, and so on).
    #[must_use]
    pub fn qualified_name(&self) -> Option<String> {
        let (name, options) = match self {
            Self::Command { name, options, .. } | Self::Autocomplete { name, options, .. } => {
                (name, options)
            }
            Self::Component { .. } | Self::Modal { .. } => return None,
        };
        let mut parts = vec![name.as_str()];
        append_qualified_option(&mut parts, options);
        Some(parts.join(" "))
    }
}

fn append_qualified_option<'a>(parts: &mut Vec<&'a str>, options: &'a [InteractionOption]) {
    if let Some(option) = options.iter().find(|option| {
        matches!(
            option.value,
            InteractionValue::Subcommand(_) | InteractionValue::SubcommandGroup(_)
        )
    }) {
        parts.push(&option.name);
        match &option.value {
            InteractionValue::Subcommand(children)
            | InteractionValue::SubcommandGroup(children) => {
                append_qualified_option(parts, children);
            }
            _ => {}
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionResponse {
    pub content: String,
    pub ephemeral: bool,
    pub allowed_mentions: InteractionAllowedMentions,
    pub embeds: Vec<InteractionEmbed>,
    pub attachments: Vec<InteractionAttachment>,
    /// Controls what an edit means when `attachments` is empty.
    ///
    /// [`InteractionAttachmentPolicy::Auto`] retains the historical transport
    /// behavior: component-only edits omit the attachment field, while other
    /// edits with no attachments serialize an empty list and clear uploads.
    /// Callers can override either side explicitly with `Clear` or `Preserve`.
    pub attachment_policy: InteractionAttachmentPolicy,
    pub components: Vec<InteractionActionRow>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InteractionAttachmentPolicy {
    /// Preserve uploads for component-only edits and clear them for other
    /// empty-attachment edits, matching the transport's historical behavior.
    #[default]
    Auto,
    /// Serialize an empty attachment list on edits, clearing existing files.
    Clear,
    /// Omit the attachment field when no replacement files are supplied.
    Preserve,
}

impl InteractionResponse {
    #[must_use]
    pub fn message(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ephemeral: false,
            allowed_mentions: InteractionAllowedMentions::Default,
            embeds: Vec::new(),
            attachments: Vec::new(),
            attachment_policy: InteractionAttachmentPolicy::Auto,
            components: Vec::new(),
        }
    }

    #[must_use]
    pub const fn ephemeral(mut self) -> Self {
        self.ephemeral = true;
        self
    }

    /// Disable every Discord mention parse for sensitive error responses.
    #[must_use]
    pub fn without_mentions(mut self) -> Self {
        self.allowed_mentions = InteractionAllowedMentions::None;
        self
    }

    /// Permit mentions only for the explicitly supplied Discord users.
    #[must_use]
    pub fn with_user_mentions(mut self, user_ids: Vec<u64>) -> Self {
        self.allowed_mentions = InteractionAllowedMentions::Users(user_ids);
        self
    }

    #[must_use]
    pub fn embed(mut self, embed: InteractionEmbed) -> Self {
        self.embeds.push(embed);
        self
    }

    #[must_use]
    pub fn attachment(mut self, attachment: InteractionAttachment) -> Self {
        self.attachments.push(attachment);
        self
    }

    /// Preserve existing Discord uploads when this response is used as an
    /// edit and no replacement attachments are supplied.
    #[must_use]
    pub const fn preserve_attachments(mut self) -> Self {
        self.attachment_policy = InteractionAttachmentPolicy::Preserve;
        self
    }

    /// Explicitly clear existing Discord uploads when this response is used
    /// as an edit.
    #[must_use]
    pub fn clear_attachments(mut self) -> Self {
        self.attachment_policy = InteractionAttachmentPolicy::Clear;
        self.attachments.clear();
        self
    }

    #[must_use]
    pub fn action_row(mut self, row: InteractionActionRow) -> Self {
        self.components.push(row);
        self
    }

    #[must_use]
    pub fn action_rows(mut self, rows: Vec<InteractionActionRow>) -> Self {
        self.components = rows;
        self
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum InteractionAllowedMentions {
    #[default]
    Default,
    None,
    Users(Vec<u64>),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InteractionButtonStyle {
    #[default]
    Primary,
    Secondary,
    Success,
    Danger,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionButton {
    pub custom_id: String,
    pub label: String,
    pub emoji: Option<String>,
    pub style: InteractionButtonStyle,
    pub disabled: bool,
}

impl InteractionButton {
    #[must_use]
    pub fn new(custom_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            custom_id: custom_id.into(),
            label: label.into(),
            emoji: None,
            style: InteractionButtonStyle::Primary,
            disabled: false,
        }
    }

    #[must_use]
    pub const fn style(mut self, style: InteractionButtonStyle) -> Self {
        self.style = style;
        self
    }

    #[must_use]
    pub fn emoji(mut self, emoji: impl Into<String>) -> Self {
        self.emoji = Some(emoji.into());
        self
    }

    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionActionRow {
    pub buttons: Vec<InteractionButton>,
    pub string_select: Option<InteractionStringSelect>,
}

impl InteractionActionRow {
    #[must_use]
    pub fn buttons(buttons: Vec<InteractionButton>) -> Self {
        Self {
            buttons,
            string_select: None,
        }
    }

    #[must_use]
    pub fn string_select(select: InteractionStringSelect) -> Self {
        Self {
            buttons: Vec::new(),
            string_select: Some(select),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionStringSelectOption {
    pub label: String,
    pub value: String,
    pub description: Option<String>,
}

impl InteractionStringSelectOption {
    #[must_use]
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            description: None,
        }
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionStringSelect {
    pub custom_id: String,
    pub placeholder: String,
    pub options: Vec<InteractionStringSelectOption>,
    pub min_values: u8,
    pub max_values: u8,
    pub disabled: bool,
}

impl InteractionStringSelect {
    #[must_use]
    pub fn new(
        custom_id: impl Into<String>,
        placeholder: impl Into<String>,
        options: Vec<InteractionStringSelectOption>,
    ) -> Self {
        Self {
            custom_id: custom_id.into(),
            placeholder: placeholder.into(),
            options,
            min_values: 1,
            max_values: 1,
            disabled: false,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InteractionEmbed {
    pub title: Option<String>,
    pub description: Option<String>,
    pub color: Option<u32>,
    pub author_name: Option<String>,
    pub author_icon_url: Option<String>,
    pub image_url: Option<String>,
    pub thumbnail_url: Option<String>,
    pub footer: Option<String>,
    pub footer_icon_url: Option<String>,
    pub fields: Vec<InteractionEmbedField>,
}

impl InteractionEmbed {
    #[must_use]
    pub fn titled(title: impl Into<String>) -> Self {
        Self {
            title: Some(title.into()),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub const fn color(mut self, color: u32) -> Self {
        self.color = Some(color);
        self
    }

    #[must_use]
    pub fn author(mut self, name: impl Into<String>, icon_url: Option<impl Into<String>>) -> Self {
        self.author_name = Some(name.into());
        self.author_icon_url = icon_url.map(Into::into);
        self
    }

    #[must_use]
    pub fn image(mut self, image_url: impl Into<String>) -> Self {
        self.image_url = Some(image_url.into());
        self
    }

    #[must_use]
    pub fn thumbnail(mut self, thumbnail_url: impl Into<String>) -> Self {
        self.thumbnail_url = Some(thumbnail_url.into());
        self
    }

    #[must_use]
    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    #[must_use]
    pub fn footer_icon(mut self, footer_icon_url: impl Into<String>) -> Self {
        self.footer_icon_url = Some(footer_icon_url.into());
        self
    }

    #[must_use]
    pub fn field(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
        inline: bool,
    ) -> Self {
        self.fields.push(InteractionEmbedField {
            name: name.into(),
            value: value.into(),
            inline,
        });
        self
    }

    /// Total characters Discord counts against the 6000-character embed cap.
    #[must_use]
    pub fn content_len(&self) -> usize {
        self.title.as_ref().map_or(0, |text| text.chars().count())
            + self
                .description
                .as_ref()
                .map_or(0, |text| text.chars().count())
            + self.footer.as_ref().map_or(0, |text| text.chars().count())
            + self
                .author_name
                .as_ref()
                .map_or(0, |text| text.chars().count())
            + self
                .fields
                .iter()
                .map(|field| field.name.chars().count() + field.value.chars().count())
                .sum::<usize>()
    }

    /// Append a field only while it fits Discord's embed limits.
    ///
    /// Discord rejects the whole message once an embed passes 6000 characters
    /// or 25 fields, so list-style embeds that grow with guild activity must
    /// stop adding rows and report the remainder in their footer instead.
    #[must_use]
    pub fn field_within_budget(
        self,
        name: impl Into<String>,
        value: impl Into<String>,
        inline: bool,
    ) -> (Self, bool) {
        let name = name.into();
        let value = value.into();
        let size = name.chars().count() + value.chars().count();
        if self.fields.len() >= EMBED_FIELD_COUNT_LIMIT
            || self.content_len() + size > EMBED_CONTENT_BUDGET
        {
            return (self, false);
        }
        (self.field(name, value, inline), true)
    }
}

/// Discord rejects an embed whose parts total more than 6000 characters.
///
/// The margin leaves room for a footer appended *after* the fields, which is
/// exactly what the callers that overflow do: a "more not shown" footer is only
/// emitted when the budget cut fired, and with several skipped categories it
/// runs past 130 characters. The margin is sized for the longest such footer
/// rather than a round number.
pub const EMBED_CONTENT_BUDGET: usize = 5_700;
/// Discord's hard limit on embed fields.
pub const EMBED_FIELD_COUNT_LIMIT: usize = 25;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionEmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionAttachment {
    pub filename: String,
    pub description: Option<String>,
    pub bytes: Vec<u8>,
}

impl InteractionAttachment {
    #[must_use]
    pub fn bytes(filename: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            filename: filename.into(),
            description: None,
            bytes: bytes.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InteractionTextInputStyle {
    #[default]
    Short,
    Paragraph,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionTextInput {
    pub custom_id: String,
    pub label: String,
    pub style: InteractionTextInputStyle,
    pub required: bool,
    pub placeholder: Option<String>,
    pub min_length: Option<u16>,
    pub max_length: Option<u16>,
    pub value: Option<String>,
}

impl InteractionTextInput {
    #[must_use]
    pub fn short(custom_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            custom_id: custom_id.into(),
            label: label.into(),
            style: InteractionTextInputStyle::Short,
            required: true,
            placeholder: None,
            min_length: None,
            max_length: None,
            value: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionModal {
    pub custom_id: String,
    pub title: String,
    pub inputs: Vec<InteractionTextInput>,
}

#[derive(Debug, Error)]
#[error("interaction response failed: {message}")]
pub struct InteractionResponseError {
    pub message: String,
    status_code: Option<u16>,
}

impl InteractionResponseError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status_code: None,
        }
    }

    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.status_code == Some(404)
    }

    pub(crate) fn with_status_code(message: impl Into<String>, status_code: Option<u16>) -> Self {
        Self {
            message: message.into(),
            status_code,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InteractionMessageDelivery {
    InteractionFollowup,
    ChannelFallback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InteractionMessageReceipt {
    pub message_id: u64,
    pub channel_id: u64,
    pub delivery: InteractionMessageDelivery,
}

#[async_trait]
pub trait InteractionResponder: Send + Sync {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError>;
    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError>;

    /// Defer while showing Discord's public/private "thinking" state.
    ///
    /// Component responders normally acknowledge with a type-6 deferred
    /// update. Paid component flows that later post a separate result opt into
    /// this type-5 response explicitly. Alternate responders retain source
    /// compatibility through the default delegation.
    async fn defer_thinking(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.defer(ephemeral).await
    }

    async fn followup(&self, response: InteractionResponse)
    -> Result<(), InteractionResponseError>;

    async fn show_modal(&self, _modal: InteractionModal) -> Result<(), InteractionResponseError> {
        Err(InteractionResponseError::new(
            "interaction responder does not support modals",
        ))
    }

    async fn autocomplete(
        &self,
        _choices: Vec<CommandOptionChoice>,
    ) -> Result<(), InteractionResponseError> {
        Err(InteractionResponseError::new(
            "interaction responder does not support autocomplete",
        ))
    }

    /// Whether an initial response has been attempted or accepted.
    ///
    /// Treating an in-flight or rejected attempt as done prevents a second
    /// initial callback after Discord reports an expired interaction token.
    fn response_is_done(&self) -> bool {
        false
    }

    /// Detailed initial-response state used by shared failure recovery.
    fn initial_response_state(&self) -> InteractionInitialResponseState {
        if self.response_is_done() {
            InteractionInitialResponseState::Accepted
        } else {
            InteractionInitialResponseState::NotStarted
        }
    }

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        self.followup(response).await?;
        Ok(None)
    }

    async fn update(&self, _response: InteractionResponse) -> Result<(), InteractionResponseError> {
        Err(InteractionResponseError::new(
            "interaction responder does not support message updates",
        ))
    }

    async fn edit_original(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        Err(InteractionResponseError::new(
            "interaction responder does not support original-message edits",
        ))
    }

    /// Edit a previously-created follow-up or channel-fallback message.
    ///
    /// The default keeps existing alternate responders source compatible;
    /// production Discord responders override it with the receipt's exact
    /// delivery mechanism.
    async fn edit_message(
        &self,
        _receipt: InteractionMessageReceipt,
        _response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        Err(InteractionResponseError::new(
            "interaction responder does not support message edits",
        ))
    }

    async fn delete_message(
        &self,
        _receipt: InteractionMessageReceipt,
    ) -> Result<(), InteractionResponseError> {
        Err(InteractionResponseError::new(
            "interaction responder does not support message deletion",
        ))
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum InteractionHandlerError {
    #[error("{0}")]
    Handler(String),
    #[error("could not transform interaction option value {value:?}")]
    Transformer { value: String },
}

impl InteractionHandlerError {
    #[must_use]
    pub const fn category(&self) -> &'static str {
        match self {
            Self::Handler(_) => "HandlerError",
            Self::Transformer { .. } => "TransformerError",
        }
    }
}

impl From<String> for InteractionHandlerError {
    fn from(message: String) -> Self {
        Self::Handler(message)
    }
}

impl From<&str> for InteractionHandlerError {
    fn from(message: &str) -> Self {
        Self::Handler(message.to_owned())
    }
}

#[async_trait]
pub trait InteractionHandler: Send + Sync {
    /// Select the transport acknowledgement policy for this request.
    ///
    /// Override this only for component buttons that call `show_modal`; modal
    /// submissions themselves should retain automatic acknowledgement.
    fn acknowledgement_policy(
        &self,
        _request: &InteractionRequest,
    ) -> InteractionAcknowledgementPolicy {
        InteractionAcknowledgementPolicy::Automatic
    }

    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError>;
}

#[derive(Clone)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    pub options: Vec<CommandOptionSpec>,
    pub handler: Arc<dyn InteractionHandler>,
}

impl fmt::Debug for CommandSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandSpec")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("options", &self.options)
            .field("handler", &"<interaction handler>")
            .finish()
    }
}

#[derive(Clone)]
pub struct ComponentRoute {
    pub custom_id_prefix: String,
    pub handler: Arc<dyn InteractionHandler>,
}

impl fmt::Debug for ComponentRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComponentRoute")
            .field("custom_id_prefix", &self.custom_id_prefix)
            .field("handler", &"<interaction handler>")
            .finish()
    }
}

pub trait RegistrationProvider: Send + Sync {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError>;
}

#[derive(Clone, Default)]
pub struct Registry {
    commands: Arc<BTreeMap<String, CommandSpec>>,
    components: Arc<Vec<ComponentRoute>>,
    global_command_sync_enabled: bool,
}

impl Registry {
    #[must_use]
    pub fn commands(&self) -> impl ExactSizeIterator<Item = &CommandSpec> {
        self.commands.values()
    }

    #[must_use]
    pub fn component_routes(&self) -> &[ComponentRoute] {
        &self.components
    }

    /// Global command replacement is destructive: Discord removes commands
    /// absent from the submitted tree. Incremental ports leave this off until
    /// the complete Python tree has a Rust provider.
    #[must_use]
    pub const fn global_command_sync_enabled(&self) -> bool {
        self.global_command_sync_enabled
    }

    #[must_use]
    pub fn command_handler(&self, name: &str) -> Option<Arc<dyn InteractionHandler>> {
        self.commands
            .get(name)
            .map(|spec| Arc::clone(&spec.handler))
    }

    #[must_use]
    pub fn component_handler(&self, custom_id: &str) -> Option<Arc<dyn InteractionHandler>> {
        self.components
            .iter()
            .find(|route| custom_id.starts_with(&route.custom_id_prefix))
            .map(|route| Arc::clone(&route.handler))
    }
}

#[derive(Default)]
pub struct RegistryBuilder {
    commands: BTreeMap<String, CommandSpec>,
    components: Vec<ComponentRoute>,
    global_command_sync_enabled: bool,
}

impl RegistryBuilder {
    pub fn command(&mut self, command: CommandSpec) -> Result<(), RegistrationError> {
        validate_command(&command)?;
        if self.commands.contains_key(&command.name) {
            return Err(RegistrationError::DuplicateCommand(command.name));
        }
        if self.commands.len() == DISCORD_GLOBAL_COMMAND_LIMIT {
            return Err(RegistrationError::TooManyCommands);
        }
        self.commands.insert(command.name.clone(), command);
        Ok(())
    }

    pub fn component(&mut self, route: ComponentRoute) -> Result<(), RegistrationError> {
        if route.custom_id_prefix.is_empty() || route.custom_id_prefix.len() > 100 {
            return Err(RegistrationError::InvalidComponentPrefix(
                route.custom_id_prefix,
            ));
        }
        if self.components.iter().any(|existing| {
            existing
                .custom_id_prefix
                .starts_with(&route.custom_id_prefix)
                || route
                    .custom_id_prefix
                    .starts_with(&existing.custom_id_prefix)
        }) {
            return Err(RegistrationError::AmbiguousComponentPrefix(
                route.custom_id_prefix,
            ));
        }
        self.components.push(route);
        self.components.sort_by(|left, right| {
            right
                .custom_id_prefix
                .len()
                .cmp(&left.custom_id_prefix.len())
        });
        Ok(())
    }

    pub fn add_provider(
        &mut self,
        provider: &dyn RegistrationProvider,
    ) -> Result<(), RegistrationError> {
        provider.register(self)
    }

    /// Permit READY handling to replace Discord's global command tree. The
    /// final cutover composition root calls this only after all extensions in
    /// the mechanical inventory are wired.
    pub const fn enable_global_command_sync(&mut self) {
        self.global_command_sync_enabled = true;
    }

    #[must_use]
    pub fn build(self) -> Registry {
        Registry {
            commands: Arc::new(self.commands),
            components: Arc::new(self.components),
            global_command_sync_enabled: self.global_command_sync_enabled,
        }
    }
}

fn validate_command(command: &CommandSpec) -> Result<(), RegistrationError> {
    validate_name(&command.name)?;
    if command.description.is_empty() || command.description.chars().count() > 100 {
        return Err(RegistrationError::InvalidDescription(command.name.clone()));
    }
    validate_options(&command.name, &command.options, 0)
}

fn validate_options(
    command_name: &str,
    options: &[CommandOptionSpec],
    depth: usize,
) -> Result<(), RegistrationError> {
    if options.len() > DISCORD_COMMAND_OPTION_LIMIT {
        return Err(RegistrationError::TooManyOptions(command_name.to_owned()));
    }
    let mut names = BTreeSet::new();
    for option in options {
        validate_name(&option.name)?;
        if !names.insert(&option.name) {
            return Err(RegistrationError::DuplicateOption(option.name.clone()));
        }
        if option.description.is_empty() || option.description.chars().count() > 100 {
            return Err(RegistrationError::InvalidDescription(option.name.clone()));
        }
        let is_group = matches!(
            option.kind,
            CommandOptionKind::Subcommand | CommandOptionKind::SubcommandGroup
        );
        if !option.options.is_empty() && !is_group {
            return Err(RegistrationError::NestedScalarOption(option.name.clone()));
        }
        if option.autocomplete
            && (!matches!(
                option.kind,
                CommandOptionKind::String | CommandOptionKind::Integer | CommandOptionKind::Number
            ) || !option.choices.is_empty())
        {
            return Err(RegistrationError::InvalidAutocompleteOption(
                option.name.clone(),
            ));
        }
        if is_group && depth >= 2 {
            return Err(RegistrationError::OptionNesting(option.name.clone()));
        }
        validate_options(command_name, &option.options, depth + 1)?;
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), RegistrationError> {
    if !(1..=32).contains(&name.chars().count())
        || !name.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'
                || character == '_'
        })
    {
        return Err(RegistrationError::InvalidName(name.to_owned()));
    }
    Ok(())
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RegistrationError {
    #[error("invalid Discord command/option name {0:?}")]
    InvalidName(String),
    #[error("invalid Discord description for {0:?}")]
    InvalidDescription(String),
    #[error("duplicate top-level command {0:?}")]
    DuplicateCommand(String),
    #[error("Discord's global top-level command limit was exceeded")]
    TooManyCommands,
    #[error("command {0:?} exceeds Discord's option limit")]
    TooManyOptions(String),
    #[error("duplicate sibling option {0:?}")]
    DuplicateOption(String),
    #[error("scalar option {0:?} cannot contain nested options")]
    NestedScalarOption(String),
    #[error("subcommand nesting exceeds Discord's supported depth at {0:?}")]
    OptionNesting(String),
    #[error("autocomplete option {0:?} must be a string, integer, or number without choices")]
    InvalidAutocompleteOption(String),
    #[error("invalid component custom-id prefix {0:?}")]
    InvalidComponentPrefix(String),
    #[error("component custom-id prefix overlaps an existing route: {0:?}")]
    AmbiguousComponentPrefix(String),
}

#[cfg(all(test, feature = "runtime-test-core"))]
mod tests {
    use super::*;

    struct Handler;

    #[async_trait]
    impl InteractionHandler for Handler {
        async fn handle(
            &self,
            _request: InteractionRequest,
            _responder: Arc<dyn InteractionResponder>,
        ) -> Result<(), InteractionHandlerError> {
            Ok(())
        }
    }

    fn command(name: &str) -> CommandSpec {
        CommandSpec {
            name: name.to_owned(),
            description: "A command".to_owned(),
            options: Vec::new(),
            handler: Arc::new(Handler),
        }
    }

    #[test]
    fn registry_rejects_duplicate_commands() {
        let mut builder = RegistryBuilder::default();
        builder.command(command("lobby")).expect("first command");
        assert_eq!(
            builder.command(command("lobby")),
            Err(RegistrationError::DuplicateCommand("lobby".to_owned()))
        );
    }

    #[test]
    fn test_summary_separates_top_level_commands_from_all_nodes() {
        let mut builder = RegistryBuilder::default();
        let dig_children = (0..24)
            .map(|index| {
                CommandOptionSpec::new(
                    format!("leaf{index}"),
                    "Dig leaf",
                    CommandOptionKind::Subcommand,
                )
            })
            .collect::<Vec<_>>();
        builder
            .command(CommandSpec {
                name: "dig".to_owned(),
                description: "Dig commands".to_owned(),
                options: dig_children,
                handler: Arc::new(Handler),
            })
            .expect("dig command");
        builder
            .command(command("profile"))
            .expect("profile command");
        let registry = builder.build();
        let commands = registry.commands().collect::<Vec<_>>();
        assert_eq!(commands.len(), 2);
        assert_eq!(
            commands
                .iter()
                .find(|command| command.name == "dig")
                .expect("dig command")
                .options
                .len(),
            24
        );
        assert_eq!(
            commands
                .iter()
                .map(|command| 1 + command.options.len())
                .sum::<usize>(),
            26
        );
    }

    #[test]
    fn test_summary_detects_duplicate_qualified_paths_only() {
        let mut builder = RegistryBuilder::default();
        builder
            .command(command("go"))
            .expect("first qualified path");
        assert_eq!(
            builder.command(command("go")),
            Err(RegistrationError::DuplicateCommand("go".to_owned()))
        );
        builder
            .command(command("profile"))
            .expect("different qualified path remains valid");
    }

    #[test]
    fn component_prefixes_must_be_unambiguous() {
        let mut builder = RegistryBuilder::default();
        builder
            .component(ComponentRoute {
                custom_id_prefix: "lobby:".to_owned(),
                handler: Arc::new(Handler),
            })
            .expect("first route");
        assert!(matches!(
            builder.component(ComponentRoute {
                custom_id_prefix: "lobby:ready:".to_owned(),
                handler: Arc::new(Handler),
            }),
            Err(RegistrationError::AmbiguousComponentPrefix(_))
        ));
    }

    #[test]
    fn global_sync_requires_an_explicit_cutover_decision() {
        let mut builder = RegistryBuilder::default();
        builder.command(command("lobby")).expect("command");
        assert!(!builder.build().global_command_sync_enabled());

        let mut builder = RegistryBuilder::default();
        builder.command(command("lobby")).expect("command");
        builder.enable_global_command_sync();
        assert!(builder.build().global_command_sync_enabled());
    }

    #[test]
    fn qualified_command_name_follows_nested_discord_subcommands() {
        let request = InteractionRequest::Command {
            interaction_id: 1,
            name: "survey".to_owned(),
            user_id: 2,
            user_display_name: "User".to_owned(),
            guild_id: Some(3),
            channel_id: Some(4),
            member_permissions: Some(8),
            options: vec![InteractionOption {
                name: "admin".to_owned(),
                value: InteractionValue::SubcommandGroup(vec![InteractionOption {
                    name: "send".to_owned(),
                    value: InteractionValue::Subcommand(Vec::new()),
                }]),
            }],
        };

        assert_eq!(request.root_name(), "survey");
        assert_eq!(
            request.qualified_name().as_deref(),
            Some("survey admin send")
        );
    }

    #[test]
    fn autocomplete_requests_preserve_the_qualified_command_and_focused_value() {
        let request = InteractionRequest::Autocomplete {
            interaction_id: 1,
            name: "dota".to_owned(),
            user_id: 2,
            guild_id: Some(3),
            channel_id: Some(4),
            focused_option: "hero_name".to_owned(),
            focused_value: "pud".to_owned(),
            options: vec![InteractionOption {
                name: "hero".to_owned(),
                value: InteractionValue::Subcommand(vec![InteractionOption {
                    name: "hero_name".to_owned(),
                    value: InteractionValue::String("pud".to_owned()),
                }]),
            }],
        };

        assert_eq!(request.root_name(), "dota");
        assert_eq!(request.qualified_name().as_deref(), Some("dota hero"));
        let InteractionRequest::Autocomplete {
            focused_option,
            focused_value,
            ..
        } = request
        else {
            panic!("autocomplete request");
        };
        assert_eq!(focused_option, "hero_name");
        assert_eq!(focused_value, "pud");
    }

    #[test]
    fn autocomplete_is_limited_to_dynamic_scalar_options() {
        let mut builder = RegistryBuilder::default();
        let invalid = CommandOptionSpec {
            choices: vec![CommandOptionChoice::String {
                name: "Pudge".to_owned(),
                value: "Pudge".to_owned(),
            }],
            ..CommandOptionSpec::new("hero", "Hero", CommandOptionKind::String).autocomplete()
        };
        let error = builder
            .command(CommandSpec {
                name: "dota".to_owned(),
                description: "Dota reference".to_owned(),
                options: vec![invalid],
                handler: Arc::new(Handler),
            })
            .unwrap_err();
        assert_eq!(
            error,
            RegistrationError::InvalidAutocompleteOption("hero".to_owned())
        );
    }
}

#[cfg(test)]
mod embed_budget_tests {
    use super::{EMBED_CONTENT_BUDGET, InteractionEmbed};

    #[test]
    fn a_filled_embed_still_fits_after_its_overflow_footer() {
        // The footer is assembled after the fields, and it is only long when
        // the budget cut fired -- the two conditions always co-occur -- so the
        // margin has to cover the longest footer a caller appends.
        let longest_footer = "more not shown: +9 open, +9 resolved, +9 cancelled  ·  \
             /predict view <id> for the full ladder  ·  /predict help for how it works";
        let mut embed =
            InteractionEmbed::titled("📈 Prediction markets").description("x".repeat(200));
        let mut added = 0;
        loop {
            let (next, accepted) = embed.field_within_budget(
                format!("📈 #{added}  ·  YES 55%  ·  vol 1234"),
                format!("\"{}\"", "w".repeat(200)),
                false,
            );
            embed = next;
            if !accepted {
                break;
            }
            added += 1;
        }

        assert!(added > 0, "the budget must admit some rows");
        assert!(embed.content_len() <= EMBED_CONTENT_BUDGET);
        let with_footer = embed.footer(longest_footer);
        assert!(
            with_footer.content_len() <= 6_000,
            "embed is {} characters once the footer is attached",
            with_footer.content_len()
        );
    }
}
