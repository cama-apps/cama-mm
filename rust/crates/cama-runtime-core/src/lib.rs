//! Serenity-independent runtime contracts and configuration.
//!
//! Provider crates share these types without depending on the concrete Discord
//! adapter, allowing Cargo to compile independent command surfaces in parallel.

#[path = "../../cama-runtime/src/application_config.rs"]
pub mod application_config;
#[path = "../../cama-runtime/src/config.rs"]
pub mod config;
#[path = "../../cama-runtime/src/discord_transport.rs"]
pub mod discord_transport;
#[path = "../../cama-runtime/src/embed_colors.rs"]
pub mod embed_colors;
#[path = "../../cama-runtime/src/gateway_events.rs"]
pub mod gateway_events;
#[path = "../../cama-runtime/src/global_hooks.rs"]
pub mod global_hooks;
#[doc(hidden)]
#[path = "../../cama-runtime/src/ids.rs"]
pub mod ids;
#[path = "../../cama-runtime/src/option_ext.rs"]
pub mod option_ext;
#[path = "../../cama-runtime/src/raw_reactions.rs"]
pub mod raw_reactions;
#[path = "../../cama-runtime/src/registration.rs"]
pub mod registration;
#[path = "../../cama-runtime/src/runtime_ports.rs"]
pub mod runtime_ports;

pub use application_config::{
    ApplicationConfig, ChannelConfig, IdentityConfig, LlmConfig, Secret, Values, config_py_env_keys,
};
pub use config::{ConfigError, DiscordToken, RuntimeConfig};
pub use gateway_events::{
    GatewayEventObserver, GatewayEventObservers, GatewayMember, GatewayObserverFailure,
    GuildMemberPageSource, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};
pub use global_hooks::{GlobalInteractionHooks, UsageMonitor, UsageSnapshot};
pub use raw_reactions::{
    RawReactionEmoji, RawReactionEvent, RawReactionKind, RawReactionObserver,
    RawReactionObserverFailure, RawReactionObservers,
};
pub use registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionActionRow, InteractionAllowedMentions, InteractionAttachment, InteractionButton,
    InteractionButtonStyle, InteractionEmbed, InteractionEmbedField, InteractionHandler,
    InteractionHandlerError, InteractionMessageDelivery, InteractionMessageReceipt,
    InteractionModal, InteractionOption, InteractionRequest, InteractionResponder,
    InteractionResponse, InteractionStringSelect, InteractionStringSelectOption,
    InteractionTextInput, InteractionTextInputStyle, InteractionValue, RegistrationError,
    RegistrationProvider, Registry, RegistryBuilder,
};
