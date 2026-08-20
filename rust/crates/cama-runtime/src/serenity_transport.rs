//! Discord gateway adapter backed by Serenity 0.12.
//!
//! Serenity owns shard heartbeats, resume URLs, identify limits, HTTP rate
//! limits, and automatic reconnects. The outer runtime supervisor still
//! restarts a fully failed client session with bounded backoff.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use cama_app::dig_bonus_events::GuildMember as DigBonusGuildMember;
use cama_app::predictions::resolve_and_neutralize_discord_mentions;
use serenity::Client;
use serenity::all::ShardStageUpdateEvent;
use serenity::all::{
    ActionRowComponent, ActivityType, AutoArchiveDuration, ButtonStyle, Channel, ChannelId,
    ChannelType, Command, CommandDataOption, CommandDataOptionValue, CommandInteraction,
    CommandOptionType, ComponentInteraction, ComponentInteractionDataKind, Context,
    CreateActionRow, CreateAllowedMentions, CreateAttachment, CreateAutocompleteResponse,
    CreateButton, CreateCommand, CreateCommandOption, CreateEmbed, CreateEmbedAuthor,
    CreateEmbedFooter, CreateInputText, CreateInteractionResponse,
    CreateInteractionResponseFollowup, CreateInteractionResponseMessage, CreateMessage,
    CreateModal, CreateSelectMenu, CreateSelectMenuKind, CreateSelectMenuOption, CreateThread,
    EditAttachments, EditInteractionResponse, EditMessage, EditThread, EventHandler,
    GatewayIntents, GetMessages, Guild, GuildChannel, GuildId, GuildMemberUpdateEvent,
    InputTextStyle, Interaction, Member, MessageId, MessageInteractionMetadata, ModalInteraction,
    Nonce, OnlineStatus, Permissions, Reaction, ReactionType, Ready, UnavailableGuild, User,
    UserId,
};
use serenity::cache::Cache;
use serenity::gateway::{ConnectionStage, ShardManager};
use serenity::http::Http;
use tokio::sync::Notify;
use tracing::{error, info, warn};

use crate::admin_provider::{AdminCommandSyncResult, AdminDiscordControl, AdminDiscordHealth};
use crate::dig_bonus_runtime::{DigBonusDiscordPort, DigBonusSendFailure};
use crate::dig_provider::{
    DigChannelSnapshot, DigDiscordPort, DigPublicHistory, DigPublicHistoryMessage,
    DigPublicSendFailure, DigPublicSendFailureKind,
};
use crate::discord_transport::{
    DiscordDirectMessageError, DiscordDirectMessageErrorKind, DiscordEmoji,
    DiscordGuildMemberSnapshot, DiscordMessage, DiscordMessageReceipt, DiscordMessageSnapshot,
    DiscordPresence, DiscordTransport, DiscordUserSnapshot,
};
use crate::duel_challenges_worker::{
    DuelChannelKind, DuelChannelSnapshot, DuelDiscordPort, DuelGuildSnapshot,
};
use crate::first_game_pool_worker::FirstGamePoolGuildSource;
use crate::gamba_guild_source::{GambaDestination, GambaGuildSource};
use crate::gateway::{
    GatewayError, GatewaySession, GatewaySessionEnd, GatewayTransport, LifecycleEvent,
};
use crate::gateway_events::{
    GatewayEventObservers, GatewayMember, GuildMemberPageSource, ReadyRecoveryContext,
};
use crate::global_hooks::GlobalInteractionHooks;
use crate::mana_provider::{ManaDiscordPort, ManaGuildMember};
use crate::pet_sweep_worker::{PetSweepDeliveryError, PetSweepDiscordPort};
use crate::player_trivia_provider::PlayerTriviaDiscordPort;
use crate::prediction_provider::{PredictionCommandDiscordPort, PredictionMarketSurface};
use crate::prediction_workers::PredictionDiscordPort;
use crate::raw_reactions::{
    RawReactionEmoji, RawReactionEvent, RawReactionKind, RawReactionObservers,
};
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec,
    InteractionAcknowledgementPolicy, InteractionActionRow, InteractionAllowedMentions,
    InteractionAttachment, InteractionAttachmentPolicy, InteractionButtonStyle, InteractionEmbed,
    InteractionHandler, InteractionInitialResponseState, InteractionMessageDelivery,
    InteractionMessageReceipt, InteractionModal, InteractionOption, InteractionRequest,
    InteractionResponder, InteractionResponse, InteractionResponseError, InteractionTextInputStyle,
    InteractionValue, Registry,
};
use crate::shop_provider::ShopDiscordPort;
use crate::survey_provider::{
    SurveyDiscordPort, SurveyDmError, SurveyDmErrorKind, SurveyDmHistory, SurveyEditError,
    SurveyEditErrorKind,
};
use crate::trivia_provider::TriviaDiscordPort;
use crate::wrapped_provider::{WrappedDiscordPort, WrappedDiscordProfile};

const WRAPPED_AVATAR_MAX_BYTES: usize = 2 * 1024 * 1024;
const WRAPPED_AVATAR_TIMEOUT: Duration = Duration::from_secs(10);
const COMPONENT_ACKNOWLEDGEMENT_DEADLINE: Duration = Duration::from_millis(1_500);
static WRAPPED_AVATAR_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

#[derive(Clone, Default)]
pub struct SerenityDiscordTransport {
    context: Arc<RwLock<Option<SerenityContext>>>,
    admin_registry: Arc<RwLock<Option<Arc<Registry>>>>,
    shard_manager: Arc<RwLock<Option<Arc<ShardManager>>>>,
    gamba_channel_id: Option<i64>,
}

#[derive(Clone)]
struct SerenityContext {
    http: Arc<Http>,
    cache: Arc<Cache>,
}

impl SerenityDiscordTransport {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_gamba_channel_id(gamba_channel_id: Option<i64>) -> Self {
        Self {
            gamba_channel_id,
            ..Self::default()
        }
    }

    fn attach(&self, context: &Context) {
        *self
            .context
            .write()
            .expect("Serenity Discord transport lock poisoned") = Some(SerenityContext {
            http: Arc::clone(&context.http),
            cache: Arc::clone(&context.cache),
        });
    }

    fn context(&self) -> Result<SerenityContext, String> {
        self.context
            .read()
            .expect("Serenity Discord transport lock poisoned")
            .clone()
            .ok_or_else(|| "Discord gateway context is not ready".to_owned())
    }

    fn attach_admin_runtime(&self, registry: Arc<Registry>, shard_manager: Arc<ShardManager>) {
        *self
            .admin_registry
            .write()
            .expect("Serenity admin registry lock poisoned") = Some(registry);
        *self
            .shard_manager
            .write()
            .expect("Serenity shard-manager lock poisoned") = Some(shard_manager);
    }

    fn admin_registry(&self) -> Result<Arc<Registry>, String> {
        self.admin_registry
            .read()
            .expect("Serenity admin registry lock poisoned")
            .clone()
            .ok_or_else(|| "Discord command registry is not attached".to_owned())
    }
}

impl std::fmt::Debug for SerenityDiscordTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SerenityDiscordTransport")
            .field(
                "attached",
                &self
                    .context
                    .read()
                    .expect("Serenity Discord transport lock poisoned")
                    .is_some(),
            )
            .field(
                "admin_registry_attached",
                &self
                    .admin_registry
                    .read()
                    .expect("Serenity admin registry lock poisoned")
                    .is_some(),
            )
            .finish()
    }
}

#[async_trait]
impl AdminDiscordControl for SerenityDiscordTransport {
    async fn health(&self) -> AdminDiscordHealth {
        let guild_count = self
            .context()
            .map_or(0, |context| context.cache.guilds().len());
        let shard_manager = self
            .shard_manager
            .read()
            .expect("Serenity shard-manager lock poisoned")
            .clone();
        let latency_ms = if let Some(shard_manager) = shard_manager {
            let runners = shard_manager.runners.lock().await;
            let latencies = runners
                .values()
                .filter_map(|runner| runner.latency)
                .collect::<Vec<_>>();
            if latencies.is_empty() {
                None
            } else {
                let total_micros = latencies.iter().map(Duration::as_micros).sum::<u128>();
                let count = u128::try_from(latencies.len()).unwrap_or(u128::MAX);
                let rounded_ms = total_micros
                    .checked_div(count)
                    .unwrap_or_default()
                    .saturating_add(500)
                    / 1_000;
                u64::try_from(rounded_ms).ok()
            }
        } else {
            None
        };
        AdminDiscordHealth {
            guild_count,
            latency_ms,
        }
    }

    async fn sync_commands(&self) -> Result<AdminCommandSyncResult, String> {
        let context = self.context()?;
        let registry = self.admin_registry()?;
        if !registry.global_command_sync_enabled() {
            return Err(
                "global command synchronization is disabled until the complete Rust tree is wired"
                    .to_owned(),
            );
        }
        if registry.commands().len() == 0 {
            return Err("refusing to replace Discord commands with an empty tree".to_owned());
        }
        let builders = || registry.commands().map(create_command).collect::<Vec<_>>();
        let mut guild_ids = context.cache.guilds();
        guild_ids.sort_unstable_by_key(|guild_id| guild_id.get());
        let guild_count = guild_ids.len();
        let mut command_count = 0usize;
        for guild_id in guild_ids {
            let synchronized = guild_id
                .set_commands(&context.http, builders())
                .await
                .map_err(|error| error.to_string())?;
            command_count = command_count.saturating_add(synchronized.len());
        }
        let synchronized = Command::set_global_commands(&context.http, builders())
            .await
            .map_err(|error| error.to_string())?;
        command_count = command_count.saturating_add(synchronized.len());
        Ok(AdminCommandSyncResult {
            command_count,
            guild_count,
        })
    }
}

impl FirstGamePoolGuildSource for SerenityDiscordTransport {
    fn live_guild_ids(&self) -> Result<Vec<i64>, String> {
        let context = self.context()?;
        context
            .cache
            .guilds()
            .into_iter()
            .map(|guild_id| {
                i64::try_from(guild_id.get())
                    .map_err(|_| format!("Discord guild id {} exceeds SQLite INTEGER", guild_id))
            })
            .collect()
    }
}

#[async_trait]
impl ShopDiscordPort for SerenityDiscordTransport {
    async fn shop_user_avatar_url(
        &self,
        guild_id: i64,
        user_id: i64,
    ) -> Result<Option<String>, String> {
        let context = self.context()?;
        let guild_id = u64::try_from(guild_id)
            .map(GuildId::new)
            .map_err(|_| "shop guild id is negative".to_owned())?;
        let user_id = u64::try_from(user_id)
            .map(UserId::new)
            .map_err(|_| "shop user id is negative".to_owned())?;
        if let Some(avatar) = context.cache.guild(guild_id).and_then(|guild| {
            guild
                .members
                .get(&user_id)
                .and_then(|member| member.user.avatar_url())
        }) {
            return Ok(Some(avatar));
        }
        match guild_id
            .member((&context.cache, context.http.as_ref()), user_id)
            .await
        {
            Ok(member) => Ok(member.user.avatar_url()),
            Err(serenity::Error::Http(error))
                if error
                    .status_code()
                    .is_some_and(|status| status.as_u16() == 404) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    async fn shop_send_temporary(
        &self,
        channel_id: i64,
        response: InteractionResponse,
        delete_after: Duration,
    ) -> Result<(), String> {
        let context = self.context()?;
        let channel_id = u64::try_from(channel_id)
            .map(ChannelId::new)
            .map_err(|_| "shop channel id is negative".to_owned())?;
        let message = channel_id
            .send_message(&context.http, channel_response(response))
            .await
            .map_err(|error| error.to_string())?;
        let http = Arc::clone(&context.http);
        tokio::spawn(async move {
            tokio::time::sleep(delete_after).await;
            if let Err(error) = channel_id.delete_message(&http, message.id).await {
                warn!(%error, "failed to delete temporary shop message");
            }
        });
        Ok(())
    }
}

#[async_trait]
impl DigDiscordPort for SerenityDiscordTransport {
    async fn dig_channel(&self, channel_id: i64) -> Result<Option<DigChannelSnapshot>, String> {
        let context = self.context()?;
        let channel_id = u64::try_from(channel_id)
            .map(ChannelId::new)
            .map_err(|_| "Dig channel id is negative".to_owned())?;
        let cached = context.cache.guilds().into_iter().find_map(|guild_id| {
            context.cache.guild(guild_id).and_then(|guild| {
                guild.channels.get(&channel_id).cloned().or_else(|| {
                    guild
                        .threads
                        .iter()
                        .find(|thread| thread.id == channel_id)
                        .cloned()
                })
            })
        });
        let channel = if let Some(channel) = cached {
            Some(channel)
        } else {
            match context.http.get_channel(channel_id).await {
                Ok(Channel::Guild(channel)) => Some(channel),
                Ok(Channel::Private(_)) => None,
                Err(serenity::Error::Http(error))
                    if error
                        .status_code()
                        .is_some_and(|status| status.as_u16() == 404) =>
                {
                    None
                }
                Err(error) => return Err(error.to_string()),
                #[allow(unreachable_patterns)]
                Ok(_) => None,
            }
        };
        Ok(channel.and_then(|channel| dig_channel_snapshot(&context, &channel)))
    }

    async fn dig_channel_is_gamba(&self, guild_id: i64, channel_id: i64) -> Result<bool, String> {
        PredictionCommandDiscordPort::prediction_channel_is_gamba(self, guild_id, channel_id).await
    }

    async fn dig_user_avatar_url(
        &self,
        guild_id: i64,
        user_id: i64,
    ) -> Result<Option<String>, String> {
        ShopDiscordPort::shop_user_avatar_url(self, guild_id, user_id).await
    }

    async fn dig_send_public(
        &self,
        channel_id: i64,
        response: InteractionResponse,
    ) -> Result<(), String> {
        let context = self.context()?;
        let channel_id = u64::try_from(channel_id)
            .map(ChannelId::new)
            .map_err(|_| "Dig channel id is negative".to_owned())?;
        channel_id
            .send_message(&context.http, channel_response(response))
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }

    async fn dig_send_public_once(
        &self,
        channel_id: i64,
        response: InteractionResponse,
        nonce: &str,
    ) -> Result<InteractionMessageReceipt, DigPublicSendFailure> {
        let context = self.context().map_err(DigPublicSendFailure::ambiguous)?;
        let channel_id = u64::try_from(channel_id)
            .map(ChannelId::new)
            .map_err(|_| DigPublicSendFailure::rejected("Dig channel id is negative"))?;
        channel_id
            .send_message(
                &context.http,
                channel_response(response)
                    .nonce(Nonce::String(nonce.to_owned()))
                    .enforce_nonce(true),
            )
            .await
            .map(|message| InteractionMessageReceipt {
                message_id: message.id.get(),
                channel_id: message.channel_id.get(),
                delivery: InteractionMessageDelivery::ChannelFallback,
            })
            .map_err(dig_public_send_failure)
    }

    async fn dig_add_reaction(
        &self,
        channel_id: i64,
        message_id: u64,
        emoji: &str,
    ) -> Result<(), String> {
        let channel_id = u64::try_from(channel_id)
            .map_err(|_| "Dig reaction channel id is negative".to_owned())?;
        DiscordTransport::add_reaction(self, channel_id, message_id, &DiscordEmoji::unicode(emoji))
            .await
    }

    async fn dig_public_history(
        &self,
        channel_id: i64,
        after_unix_seconds: i64,
        limit: usize,
    ) -> Result<DigPublicHistory, String> {
        let context = self.context()?;
        let channel_id = u64::try_from(channel_id)
            .map(ChannelId::new)
            .map_err(|_| "Dig channel id is negative".to_owned())?;
        let bot_user_id = context
            .http
            .get_current_user()
            .await
            .map_err(|error| error.to_string())?
            .id
            .get();
        let mut before = None;
        let mut messages = Vec::new();
        while messages.len() < limit {
            let page_limit = (limit - messages.len()).min(100) as u8;
            let mut request = GetMessages::new().limit(page_limit);
            if let Some(message_id) = before {
                request = request.before(message_id);
            }
            let page = channel_id
                .messages(&context.http, request)
                .await
                .map_err(|error| error.to_string())?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            let oldest_before_cutoff = page
                .last()
                .is_some_and(|message| message.timestamp.unix_timestamp() < after_unix_seconds);
            before = page.last().map(|message| message.id);
            for message in page {
                if message.timestamp.unix_timestamp() < after_unix_seconds {
                    continue;
                }
                messages.push(DigPublicHistoryMessage {
                    message_id: message.id.get(),
                    author_id: message.author.id.get(),
                    nonce: message.nonce.map(|nonce| match nonce {
                        Nonce::String(value) => value,
                        Nonce::Number(value) => value.to_string(),
                    }),
                    interaction_id: message
                        .interaction_metadata
                        .as_deref()
                        .and_then(|metadata| match metadata {
                            MessageInteractionMetadata::Command(metadata) => {
                                Some(metadata.id.get())
                            }
                            MessageInteractionMetadata::Component(metadata) => {
                                Some(metadata.id.get())
                            }
                            MessageInteractionMetadata::ModalSubmit(metadata) => {
                                Some(metadata.id.get())
                            }
                            MessageInteractionMetadata::Unknown(_) => None,
                            _ => None,
                        }),
                    content: message.content,
                    embed_titles: message
                        .embeds
                        .iter()
                        .map(|embed| embed.title.clone())
                        .collect(),
                    embed_descriptions: message
                        .embeds
                        .iter()
                        .map(|embed| embed.description.clone())
                        .collect(),
                });
            }
            if page_len < usize::from(page_limit) || oldest_before_cutoff {
                break;
            }
        }
        Ok(DigPublicHistory {
            bot_user_id,
            messages,
        })
    }

    async fn dig_send_temporary(
        &self,
        channel_id: i64,
        response: InteractionResponse,
        delete_after: Duration,
    ) -> Result<(), String> {
        ShopDiscordPort::shop_send_temporary(self, channel_id, response, delete_after).await
    }
}

#[async_trait]
impl DigBonusDiscordPort for SerenityDiscordTransport {
    async fn guild_members(&self, guild_id: i64) -> Result<Vec<DigBonusGuildMember>, String> {
        let context = self.context()?;
        let guild_id = u64::try_from(guild_id)
            .map(GuildId::new)
            .map_err(|_| "Dig bonus guild id is negative".to_owned())?;
        let guild = context.cache.guild(guild_id).ok_or_else(|| {
            "Dig bonus guild is unavailable from Discord's authoritative member cache".to_owned()
        })?;
        guild
            .members
            .values()
            .map(|member| {
                Ok(DigBonusGuildMember {
                    id: i64::try_from(member.user.id.get())
                        .map_err(|_| "Dig bonus member id exceeds SQLite INTEGER".to_owned())?,
                    display_name: member.display_name().to_owned(),
                    bot: member.user.bot,
                })
            })
            .collect()
    }

    async fn send_bonus(
        &self,
        responder: Arc<dyn InteractionResponder>,
        response: InteractionResponse,
    ) -> Result<InteractionMessageReceipt, DigBonusSendFailure> {
        responder
            .followup_with_receipt(response)
            .await
            .map_err(|error| DigBonusSendFailure::Ambiguous(error.to_string()))?
            .ok_or_else(|| {
                DigBonusSendFailure::Ambiguous(
                    "Dig bonus follow-up did not return a message receipt".to_owned(),
                )
            })
    }

    async fn edit_bonus_message(
        &self,
        receipt: InteractionMessageReceipt,
        response: InteractionResponse,
    ) -> Result<(), String> {
        let context = self.context()?;
        ChannelId::new(receipt.channel_id)
            .edit_message(
                (&context.cache, context.http.as_ref()),
                MessageId::new(receipt.message_id),
                edit_channel_response(DiscordMessage::default_mentions(response)),
            )
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

#[async_trait]
impl SurveyDiscordPort for SerenityDiscordTransport {
    async fn survey_guild_snapshot(
        &self,
        guild_id: i64,
    ) -> Result<cama_app::survey_commands::GuildSnapshot, String> {
        let context = self.context()?;
        let guild_id = u64::try_from(guild_id)
            .map(GuildId::new)
            .map_err(|_| "survey guild id is negative".to_owned())?;
        let guild = context.cache.guild(guild_id).ok_or_else(|| {
            "survey guild is unavailable from Discord's authoritative member cache".to_owned()
        })?;
        let signed_guild = i64::try_from(guild_id.get())
            .map_err(|_| "survey guild id exceeds SQLite INTEGER".to_owned())?;
        let mut members = guild
            .members
            .values()
            .map(|member| {
                Ok(cama_app::survey_commands::MemberSnapshot {
                    discord_id: i64::try_from(member.user.id.get())
                        .map_err(|_| "survey member id exceeds SQLite INTEGER".to_owned())?,
                    guild_id: signed_guild,
                    bot: member.user.bot,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        members.sort_unstable_by_key(|member| member.discord_id);
        Ok(cama_app::survey_commands::GuildSnapshot {
            guild_id: signed_guild,
            members,
        })
    }

    async fn survey_role_snapshot(
        &self,
        guild_id: i64,
        role_id: i64,
    ) -> Result<Option<cama_app::survey_commands::RoleSnapshot>, String> {
        let context = self.context()?;
        let guild_id = u64::try_from(guild_id)
            .map(GuildId::new)
            .map_err(|_| "survey guild id is negative".to_owned())?;
        let role_id = u64::try_from(role_id)
            .map(serenity::all::RoleId::new)
            .map_err(|_| "survey role id is negative".to_owned())?;
        let guild = context.cache.guild(guild_id).ok_or_else(|| {
            "survey guild is unavailable from Discord's authoritative member cache".to_owned()
        })?;
        let Some(role) = guild.roles.get(&role_id) else {
            return Ok(None);
        };
        if role.guild_id != guild_id {
            return Ok(None);
        }
        let signed_guild = i64::try_from(guild_id.get())
            .map_err(|_| "survey guild id exceeds SQLite INTEGER".to_owned())?;
        let mut members = guild
            .members
            .values()
            .filter(|member| member.roles.contains(&role_id))
            .map(|member| {
                Ok(cama_app::survey_commands::MemberSnapshot {
                    discord_id: i64::try_from(member.user.id.get())
                        .map_err(|_| "survey member id exceeds SQLite INTEGER".to_owned())?,
                    guild_id: signed_guild,
                    bot: member.user.bot,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        members.sort_unstable_by_key(|member| member.discord_id);
        Ok(Some(cama_app::survey_commands::RoleSnapshot {
            guild_id: signed_guild,
            members,
        }))
    }

    async fn survey_send_dm(
        &self,
        discord_id: i64,
        message: DiscordMessage,
        nonce: &str,
    ) -> Result<DiscordMessageReceipt, SurveyDmError> {
        let context = self.context().map_err(|message| SurveyDmError {
            kind: SurveyDmErrorKind::Unavailable,
            message,
        })?;
        let discord_id = u64::try_from(discord_id)
            .map(UserId::new)
            .map_err(|_| SurveyDmError {
                kind: SurveyDmErrorKind::Unavailable,
                message: "survey recipient id is negative".to_owned(),
            })?;
        let response = apply_discord_mentions(message.response.clone(), &message);
        discord_id
            .direct_message(
                (&context.cache, context.http.as_ref()),
                channel_response(response)
                    .nonce(Nonce::String(nonce.to_owned()))
                    .enforce_nonce(true),
            )
            .await
            .map(|sent| DiscordMessageReceipt {
                channel_id: sent.channel_id.get(),
                message_id: sent.id.get(),
                jump_url: sent.link(),
            })
            .map_err(|error| SurveyDmError {
                kind: match &error {
                    serenity::Error::Http(http)
                        if http
                            .status_code()
                            .is_some_and(|status| status.as_u16() == 403) =>
                    {
                        SurveyDmErrorKind::Forbidden
                    }
                    _ => SurveyDmErrorKind::Unavailable,
                },
                message: error.to_string(),
            })
    }

    async fn survey_dm_history(
        &self,
        discord_id: i64,
        after_unix_seconds: i64,
        limit: usize,
    ) -> Result<SurveyDmHistory, String> {
        let context = self.context()?;
        let user_id = u64::try_from(discord_id)
            .map(UserId::new)
            .map_err(|_| "survey recipient id is negative".to_owned())?;
        let user = user_id
            .to_user((&context.cache, context.http.as_ref()))
            .await
            .map_err(|error| error.to_string())?;
        let channel = user
            .create_dm_channel((&context.cache, context.http.as_ref()))
            .await
            .map_err(|error| error.to_string())?;
        let bot_user_id = context
            .http
            .get_current_user()
            .await
            .map_err(|error| error.to_string())?;
        let mut before = None;
        let mut messages = Vec::new();
        while messages.len() < limit {
            let page_limit = (limit - messages.len()).min(100) as u8;
            let mut request = GetMessages::new().limit(page_limit);
            if let Some(message_id) = before {
                request = request.before(message_id);
            }
            let page = channel
                .id
                .messages((&context.cache, context.http.as_ref()), request)
                .await
                .map_err(|error| error.to_string())?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            let oldest_before_cutoff = page
                .last()
                .is_some_and(|message| message.timestamp.unix_timestamp() < after_unix_seconds);
            before = page.last().map(|message| message.id);
            for message in page {
                if message.timestamp.unix_timestamp() < after_unix_seconds {
                    continue;
                }
                let components = message
                    .components
                    .iter()
                    .flat_map(|row| row.components.iter())
                    .filter_map(|component| match component {
                        ActionRowComponent::Button(button) => match &button.data {
                            serenity::all::ButtonKind::NonLink { custom_id, .. } => {
                                Some(custom_id.clone())
                            }
                            _ => None,
                        },
                        ActionRowComponent::SelectMenu(select) => select.custom_id.clone(),
                        ActionRowComponent::InputText(_) => None,
                        _ => None,
                    })
                    .map(|custom_id| cama_app::survey_commands::MessageComponent {
                        custom_id: Some(custom_id),
                    })
                    .collect();
                messages.push(cama_app::survey_commands::HistoryMessage {
                    message_id: i64::try_from(message.id.get())
                        .map_err(|_| "survey DM id exceeds SQLite INTEGER".to_owned())?,
                    author_id: i64::try_from(message.author.id.get())
                        .map_err(|_| "survey DM author id exceeds SQLite INTEGER".to_owned())?,
                    nonce: None,
                    components,
                });
            }
            if page_len < usize::from(page_limit) || oldest_before_cutoff {
                break;
            }
        }
        Ok(SurveyDmHistory {
            channel_id: i64::try_from(channel.id.get())
                .map_err(|_| "survey DM channel id exceeds SQLite INTEGER".to_owned())?,
            bot_user_id: i64::try_from(bot_user_id.id.get())
                .map_err(|_| "survey bot id exceeds SQLite INTEGER".to_owned())?,
            messages,
        })
    }

    async fn survey_edit_dm(
        &self,
        channel_id: i64,
        message_id: i64,
        message: DiscordMessage,
    ) -> Result<(), SurveyEditError> {
        let unavailable = |message| SurveyEditError {
            kind: SurveyEditErrorKind::Unavailable,
            message,
        };
        let context = self.context().map_err(unavailable)?;
        let channel_id = u64::try_from(channel_id)
            .map(ChannelId::new)
            .map_err(|_| unavailable("survey DM channel id is negative".to_owned()))?;
        let message_id = u64::try_from(message_id)
            .map(MessageId::new)
            .map_err(|_| unavailable("survey DM message id is negative".to_owned()))?;
        channel_id
            .edit_message(
                (&context.cache, context.http.as_ref()),
                message_id,
                edit_channel_response(message),
            )
            .await
            .map(drop)
            .map_err(|error| SurveyEditError {
                kind: match &error {
                    serenity::Error::Http(http)
                        if http
                            .status_code()
                            .is_some_and(|status| status.as_u16() == 403) =>
                    {
                        SurveyEditErrorKind::Forbidden
                    }
                    serenity::Error::Http(http)
                        if http
                            .status_code()
                            .is_some_and(|status| status.as_u16() == 404) =>
                    {
                        SurveyEditErrorKind::NotFound
                    }
                    _ => SurveyEditErrorKind::Unavailable,
                },
                message: error.to_string(),
            })
    }
}

impl GambaGuildSource for SerenityDiscordTransport {
    fn live_gamba_destinations(&self) -> Result<Vec<GambaDestination>, String> {
        let context = self.context()?;
        let mut destinations = Vec::new();
        let mut guild_ids = context.cache.guilds();
        guild_ids.sort_unstable_by_key(|guild_id| guild_id.get());
        for guild_id in guild_ids {
            let Some((raw_guild_id, mut channels)) = context.cache.guild(guild_id).map(|guild| {
                (
                    guild.id.get(),
                    guild.channels.values().cloned().collect::<Vec<_>>(),
                )
            }) else {
                continue;
            };
            channels.sort_unstable_by_key(|channel| (channel.position, channel.id.get()));
            let configured_id = self
                .gamba_channel_id
                .and_then(|channel_id| u64::try_from(channel_id).ok());
            let Some(channel) = channels
                .iter()
                .find(|channel| {
                    configured_id == Some(channel.id.get())
                        && matches!(channel.kind, ChannelType::Text | ChannelType::News)
                })
                .or_else(|| {
                    channels.iter().find(|channel| {
                        matches!(channel.kind, ChannelType::Text | ChannelType::News)
                            && channel.name.to_lowercase().contains("gamba")
                    })
                })
            else {
                continue;
            };
            destinations.push(GambaDestination {
                guild_id: i64::try_from(raw_guild_id)
                    .map_err(|_| "Discord guild ID exceeds SQLite integer range".to_owned())?,
                channel_id: i64::try_from(channel.id.get())
                    .map_err(|_| "Discord channel ID exceeds SQLite integer range".to_owned())?,
            });
        }
        Ok(destinations)
    }
}

#[derive(Debug, Default)]
pub struct SerenityGateway;

#[derive(Debug)]
pub struct SerenityGatewayWithDiscord {
    discord_transport: Arc<SerenityDiscordTransport>,
}

impl SerenityGateway {
    #[must_use]
    pub fn with_discord_transport(
        discord_transport: Arc<SerenityDiscordTransport>,
    ) -> SerenityGatewayWithDiscord {
        SerenityGatewayWithDiscord { discord_transport }
    }
}

#[async_trait]
impl GatewayTransport for SerenityGateway {
    async fn run_session(
        &mut self,
        session: GatewaySession,
    ) -> Result<GatewaySessionEnd, GatewayError> {
        run_serenity_session(session, None).await
    }
}

#[async_trait]
impl GatewayTransport for SerenityGatewayWithDiscord {
    async fn run_session(
        &mut self,
        session: GatewaySession,
    ) -> Result<GatewaySessionEnd, GatewayError> {
        run_serenity_session(session, Some(Arc::clone(&self.discord_transport))).await
    }
}

async fn run_serenity_session(
    session: GatewaySession,
    discord_transport: Option<Arc<SerenityDiscordTransport>>,
) -> Result<GatewaySessionEnd, GatewayError> {
    let registry = Arc::clone(&session.registry);
    let admin_discord_transport = discord_transport.clone();
    let handler = SerenityHandler {
        registry: Arc::clone(&registry),
        events: session.events.clone(),
        observers: session.observers,
        global_interaction_hooks: session.global_interaction_hooks,
        raw_reaction_observers: session.raw_reaction_observers,
        discord_transport,
        bot_user_id: AtomicU64::new(0),
        guild_ids: Arc::new(RwLock::new(BTreeSet::new())),
    };
    let intents = serenity_intents(session.intents);
    let mut client = Client::builder(session.token.expose(), intents)
        .event_handler(handler)
        .await
        .map_err(|error| GatewayError::Fatal(error.to_string()))?;
    let shard_manager = Arc::clone(&client.shard_manager);
    if let Some(discord_transport) = admin_discord_transport {
        discord_transport.attach_admin_runtime(registry, Arc::clone(&shard_manager));
    }
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    let shutdown_requested_for_task = Arc::clone(&shutdown_requested);
    let mut shutdown = session.shutdown;
    let shutdown_task = tokio::spawn(async move {
        loop {
            match shutdown.changed().await {
                Ok(()) if !*shutdown.borrow() => continue,
                Ok(()) | Err(_) => {
                    shutdown_requested_for_task.store(true, Ordering::Release);
                    shard_manager.shutdown_all().await;
                    break;
                }
            }
        }
    });

    let result = client.start().await;
    shutdown_task.abort();
    if shutdown_requested.load(Ordering::Acquire) {
        return Ok(GatewaySessionEnd::Shutdown);
    }
    result
        .map(|()| GatewaySessionEnd::Reconnect {
            reason: "Discord client returned without a shutdown request".to_owned(),
        })
        .map_err(|error| GatewayError::Recoverable(error.to_string()))
}

fn serenity_intents(profile: crate::gateway::GatewayIntentProfile) -> GatewayIntents {
    let mut intents = GatewayIntents::empty();
    if profile.guilds {
        // `discord.Intents.default()` includes every non-privileged intent.
        intents |= GatewayIntents::non_privileged();
    }
    if profile.guild_members {
        intents |= GatewayIntents::GUILD_MEMBERS;
    }
    if profile.guild_presences {
        intents |= GatewayIntents::GUILD_PRESENCES;
    }
    if profile.guild_messages {
        intents |= GatewayIntents::GUILD_MESSAGES;
    }
    if profile.guild_message_reactions {
        intents |= GatewayIntents::GUILD_MESSAGE_REACTIONS;
    }
    if profile.direct_messages {
        intents |= GatewayIntents::DIRECT_MESSAGES;
    }
    if profile.message_content {
        intents |= GatewayIntents::MESSAGE_CONTENT;
    }
    intents
}

struct SerenityHandler {
    registry: Arc<Registry>,
    events: tokio::sync::broadcast::Sender<LifecycleEvent>,
    observers: GatewayEventObservers,
    global_interaction_hooks: Option<GlobalInteractionHooks>,
    raw_reaction_observers: RawReactionObservers,
    discord_transport: Option<Arc<SerenityDiscordTransport>>,
    bot_user_id: AtomicU64,
    guild_ids: Arc<RwLock<BTreeSet<u64>>>,
}

#[derive(Debug, Eq, PartialEq)]
struct GlobalCommandSyncOutcome {
    command_count: usize,
    synchronized: bool,
    error: Option<String>,
}

impl GlobalCommandSyncOutcome {
    fn lifecycle_event(&self, component_route_count: usize) -> LifecycleEvent {
        LifecycleEvent::CommandsRegistered {
            command_count: self.command_count,
            component_route_count,
            synchronized: self.synchronized,
        }
    }
}

impl SerenityHandler {
    /// Apply the complete composed command tree only after the cutover gate
    /// has been enabled.  Keeping the HTTP operation and its lifecycle result
    /// together prevents a failed Discord REST request from being reported as
    /// a synchronized command tree.
    async fn synchronize_global_commands(&self, http: &Http) -> GlobalCommandSyncOutcome {
        let commands = self
            .registry
            .commands()
            .map(create_command)
            .collect::<Vec<_>>();
        let command_count = commands.len();
        let (synchronized, error) = if !self.registry.global_command_sync_enabled() {
            info!(
                command_count,
                "global command synchronization disabled until the complete command tree is wired"
            );
            (false, None)
        } else if commands.is_empty() {
            error!(
                "no Rust command providers registered; preserving Discord's existing global command tree"
            );
            (
                false,
                Some("refusing to synchronize an empty command tree".to_owned()),
            )
        } else {
            match Command::set_global_commands(http, commands).await {
                Ok(_) => (true, None),
                Err(error) => {
                    let message = error.to_string();
                    error!(%message, "failed to synchronize global Discord commands");
                    (false, Some(message))
                }
            }
        };
        GlobalCommandSyncOutcome {
            command_count,
            synchronized,
            error,
        }
    }
}

#[derive(Debug)]
struct SerenityGuildMemberPageSource {
    http: Arc<Http>,
}

#[async_trait]
impl GuildMemberPageSource for SerenityGuildMemberPageSource {
    async fn fetch_page(
        &self,
        guild_id: u64,
        after: Option<u64>,
        limit: u64,
    ) -> Result<Vec<GatewayMember>, String> {
        GuildId::new(guild_id)
            .members(&self.http, Some(limit), after.map(UserId::new))
            .await
            .map_err(|error| error.to_string())
            .map(|members| members.iter().map(gateway_member).collect())
    }
}

#[async_trait]
impl EventHandler for SerenityHandler {
    async fn shard_stage_update(&self, _context: Context, event: ShardStageUpdateEvent) {
        let stage = format!("{:?}", event.new).to_lowercase();
        let _ = self.events.send(LifecycleEvent::GatewayShardStageChanged {
            shard_id: event.shard_id.get(),
            connected: matches!(event.new, ConnectionStage::Connected),
            stage,
        });
    }

    async fn ready(&self, context: Context, ready: Ready) {
        self.attach_discord_transport(&context);
        self.bot_user_id
            .store(ready.user.id.get(), Ordering::Release);
        let guild_ids = ready
            .guilds
            .iter()
            .map(|guild| guild.id.get())
            .collect::<BTreeSet<_>>();
        *self
            .guild_ids
            .write()
            .expect("gateway guild-id lock poisoned") = guild_ids.clone();
        self.run_ready_recovery(context.http.clone(), guild_ids.into_iter().collect())
            .await;

        // Never erase the Python command tree if a development build connects
        // before every Rust command provider has been wired.  The result is
        // emitted only after the REST operation has completed, so a rejected
        // request cannot be reported as synchronized.
        let command_sync = self.synchronize_global_commands(&context.http).await;
        let _ = self.events.send(LifecycleEvent::Ready {
            bot_user_id: ready.user.id.get(),
            guild_count: ready.guilds.len(),
        });
        let _ = self
            .events
            .send(command_sync.lifecycle_event(self.registry.component_routes().len()));
        info!(
            bot_user_id = ready.user.id.get(),
            guild_count = ready.guilds.len(),
            command_count = command_sync.command_count,
            synchronized = command_sync.synchronized,
            "Discord gateway ready"
        );
    }

    async fn resume(&self, context: Context, _event: serenity::all::ResumedEvent) {
        self.attach_discord_transport(&context);
        let guild_ids = self
            .guild_ids
            .read()
            .expect("gateway guild-id lock poisoned")
            .iter()
            .copied()
            .collect();
        self.run_ready_recovery(context.http, guild_ids).await;
        let _ = self.events.send(LifecycleEvent::Resumed);
        info!("Discord gateway session resumed");
    }

    async fn guild_create(&self, context: Context, guild: Guild, _is_new: Option<bool>) {
        self.attach_discord_transport(&context);
        let guild_id = guild.id.get();
        let inserted = self
            .guild_ids
            .write()
            .expect("gateway guild-id lock poisoned")
            .insert(guild_id);
        if inserted {
            self.run_ready_recovery(context.http, vec![guild_id]).await;
        }
    }

    async fn guild_delete(
        &self,
        _context: Context,
        incomplete: UnavailableGuild,
        _full: Option<Guild>,
    ) {
        if !incomplete.unavailable {
            self.guild_ids
                .write()
                .expect("gateway guild-id lock poisoned")
                .remove(&incomplete.id.get());
        }
    }

    async fn guild_member_addition(&self, _context: Context, member: Member) {
        self.log_observer_failures(
            "member join",
            self.observers.member_join(gateway_member(&member)).await,
        );
    }

    async fn guild_member_update(
        &self,
        _context: Context,
        _old: Option<Member>,
        _new: Option<Member>,
        event: GuildMemberUpdateEvent,
    ) {
        self.log_observer_failures(
            "member update",
            self.observers
                .member_update(gateway_member_update(&event))
                .await,
        );
    }

    async fn guild_member_removal(
        &self,
        _context: Context,
        guild_id: GuildId,
        user: User,
        _member: Option<Member>,
    ) {
        self.log_observer_failures(
            "member removal",
            self.observers
                .member_remove(guild_id.get(), user.id.get())
                .await,
        );
    }

    async fn interaction_create(&self, context: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(command) => self.dispatch_command(context, command).await,
            Interaction::Autocomplete(command) => {
                self.dispatch_autocomplete(context, command).await;
            }
            Interaction::Component(component) => self.dispatch_component(context, component).await,
            Interaction::Modal(modal) => self.dispatch_modal(context, modal).await,
            _ => {}
        }
    }

    async fn reaction_add(&self, _context: Context, reaction: Reaction) {
        self.dispatch_raw_reaction(RawReactionKind::Add, reaction)
            .await;
    }

    async fn reaction_remove(&self, _context: Context, reaction: Reaction) {
        self.dispatch_raw_reaction(RawReactionKind::Remove, reaction)
            .await;
    }
}

impl SerenityHandler {
    fn attach_discord_transport(&self, context: &Context) {
        if let Some(discord_transport) = &self.discord_transport {
            discord_transport.attach(context);
        }
    }
    async fn dispatch_raw_reaction(&self, kind: RawReactionKind, reaction: Reaction) {
        if self.raw_reaction_observers.is_empty() {
            return;
        }
        let bot_user_id = self.bot_user_id.load(Ordering::Acquire);
        let Some(user_id) = reaction.user_id.map(|id| id.get()) else {
            return;
        };
        // Python ignores raw reactions until its own gateway user is ready and
        // always ignores reactions created by that user.
        if bot_user_id == 0 || user_id == bot_user_id {
            return;
        }
        let actor_is_bot = reaction.member.as_ref().map(|member| member.user.bot);
        let actor_display_name = reaction
            .member
            .as_ref()
            .map(|member| member.display_name().to_owned());
        let event = RawReactionEvent {
            kind,
            guild_id: reaction.guild_id.map(|id| id.get()),
            channel_id: reaction.channel_id.get(),
            message_id: reaction.message_id.get(),
            user_id,
            actor_is_bot,
            actor_display_name,
            emoji: gateway_reaction_emoji(&reaction.emoji),
        };
        for failure in self.raw_reaction_observers.dispatch(event).await {
            error!(
                observer = failure.observer,
                error = %failure.message,
                ?kind,
                "Discord raw-reaction observer failed"
            );
        }
    }

    async fn run_ready_recovery(&self, http: Arc<Http>, guild_ids: Vec<u64>) {
        if self.observers.is_empty() {
            return;
        }
        let member_source: Arc<dyn GuildMemberPageSource> =
            Arc::new(SerenityGuildMemberPageSource { http });
        let reports = self
            .observers
            .ready_recovery(ReadyRecoveryContext::new(
                Arc::<[u64]>::from(guild_ids),
                member_source,
            ))
            .await;
        for report in reports {
            for failure in &report.failures {
                error!(
                    observer = report.observer,
                    guild_id = failure.guild_id,
                    error = %failure.message,
                    "Discord ready-recovery observer failed for guild"
                );
            }
            let _ = self.events.send(LifecycleEvent::ReadyRecoveryCompleted {
                observer: report.observer.to_owned(),
                guilds_attempted: report.guilds_attempted,
                guilds_refreshed: report.guilds_refreshed,
                guilds_superseded: report.guilds_superseded,
                members_refreshed: report.members_refreshed,
                failure_count: report.failures.len(),
            });
        }
    }

    fn log_observer_failures(
        &self,
        event: &'static str,
        failures: Vec<crate::gateway_events::GatewayObserverFailure>,
    ) {
        for failure in failures {
            error!(
                observer = failure.observer,
                error = %failure.message,
                event,
                "Discord gateway event observer failed"
            );
        }
    }

    async fn dispatch_command(&self, context: Context, command: CommandInteraction) {
        let request = InteractionRequest::Command {
            interaction_id: command.id.get(),
            name: command.data.name.clone(),
            user_id: command.user.id.get(),
            user_display_name: command
                .member
                .as_deref()
                .map_or_else(|| command.user.display_name(), Member::display_name)
                .to_owned(),
            guild_id: command.guild_id.map(|id| id.get()),
            channel_id: Some(command.channel_id.get()),
            member_permissions: command
                .member
                .as_deref()
                .and_then(|member| member.permissions)
                .map(|permissions| permissions.bits()),
            options: command
                .data
                .options
                .iter()
                .map(|option| interaction_option(option, &command))
                .collect(),
        };
        let qualified_name = request.qualified_name();
        let handler = self.registry.command_handler(&command.data.name);
        if let Some(hooks) = &self.global_interaction_hooks {
            hooks.observe_command(handler.as_ref().and(qualified_name.as_deref()));
        }
        let Some(handler) = handler else {
            warn!(command = %command.data.name, "unregistered Discord command received");
            return;
        };
        let responder = coordinated_responder(Arc::new(CommandResponder {
            interaction: command,
            http: context.http,
            acknowledged: AtomicBool::new(false),
        }));
        run_command_handler(
            handler,
            request,
            qualified_name.as_deref(),
            responder,
            self.global_interaction_hooks.as_ref(),
        )
        .await;
    }

    async fn dispatch_autocomplete(&self, context: Context, command: CommandInteraction) {
        let Some(focused) = command.data.autocomplete() else {
            warn!(command = %command.data.name, "autocomplete interaction had no focused option");
            return;
        };
        let request = InteractionRequest::Autocomplete {
            interaction_id: command.id.get(),
            name: command.data.name.clone(),
            user_id: command.user.id.get(),
            guild_id: command.guild_id.map(|id| id.get()),
            channel_id: Some(command.channel_id.get()),
            focused_option: focused.name.to_owned(),
            focused_value: focused.value.to_owned(),
            options: command
                .data
                .options
                .iter()
                .map(|option| interaction_option(option, &command))
                .collect(),
        };
        let Some(handler) = self.registry.command_handler(&command.data.name) else {
            warn!(command = %command.data.name, "unregistered Discord autocomplete received");
            return;
        };
        let responder = coordinated_responder(Arc::new(CommandResponder {
            interaction: command,
            http: context.http,
            acknowledged: AtomicBool::new(false),
        }));
        if let Err(error) = handler.handle(request, Arc::clone(&responder)).await {
            warn!(%error, "autocomplete handler failed");
            if !responder.response_is_done() {
                let _ = responder.autocomplete(Vec::new()).await;
            }
        }
    }

    async fn dispatch_component(&self, context: Context, component: ComponentInteraction) {
        let custom_id = component.data.custom_id.clone();
        let request = InteractionRequest::Component {
            interaction_id: component.id.get(),
            custom_id: custom_id.clone(),
            user_id: component.user.id.get(),
            user_display_name: component
                .member
                .as_ref()
                .map_or_else(|| component.user.display_name(), Member::display_name)
                .to_owned(),
            guild_id: component.guild_id.map(|id| id.get()),
            channel_id: Some(component.channel_id.get()),
            member_permissions: component
                .member
                .as_ref()
                .and_then(|member| member.permissions)
                .map(|permissions| permissions.bits()),
            values: component_values(&component.data.kind),
        };
        let responder = coordinated_responder(Arc::new(ComponentResponder {
            interaction: component,
            http: context.http,
            acknowledged: AtomicBool::new(false),
        }));
        let Some(handler) = self.registry.component_handler(&custom_id) else {
            warn!(%custom_id, "unregistered component received");
            let _ = responder
                .respond(
                    InteractionResponse::message("This control is no longer active.").ephemeral(),
                )
                .await;
            return;
        };
        run_component_handler(handler, request, responder).await;
    }

    async fn dispatch_modal(&self, context: Context, modal: ModalInteraction) {
        let custom_id = modal.data.custom_id.clone();
        let fields = modal
            .data
            .components
            .iter()
            .flat_map(|row| row.components.iter())
            .filter_map(|component| match component {
                ActionRowComponent::InputText(input) => input
                    .value
                    .as_ref()
                    .map(|value| (input.custom_id.clone(), value.clone())),
                _ => None,
            })
            .collect();
        let request = InteractionRequest::Modal {
            interaction_id: modal.id.get(),
            custom_id: custom_id.clone(),
            user_id: modal.user.id.get(),
            guild_id: modal.guild_id.map(|id| id.get()),
            channel_id: Some(modal.channel_id.get()),
            member_permissions: modal
                .member
                .as_ref()
                .and_then(|member| member.permissions)
                .map(|permissions| permissions.bits()),
            fields,
        };
        let responder = coordinated_responder(Arc::new(ModalResponder {
            interaction: modal,
            http: context.http,
            acknowledged: AtomicBool::new(false),
        }));
        let Some(handler) = self.registry.component_handler(&custom_id) else {
            warn!(%custom_id, "unregistered modal received");
            let _ = responder
                .respond(InteractionResponse::message("This form is no longer active.").ephemeral())
                .await;
            return;
        };
        run_component_handler(handler, request, responder).await;
    }
}

fn gateway_reaction_emoji(emoji: &ReactionType) -> RawReactionEmoji {
    match emoji {
        ReactionType::Custom { animated, id, name } => {
            RawReactionEmoji::custom(id.get(), name.clone().unwrap_or_default(), *animated)
        }
        ReactionType::Unicode(name) => RawReactionEmoji::unicode(name),
        _ => RawReactionEmoji::unicode(emoji.to_string()),
    }
}

fn gateway_member(member: &Member) -> GatewayMember {
    GatewayMember::new(
        member.guild_id.get(),
        member.user.id.get(),
        member.nick.clone(),
    )
}

fn gateway_member_update(event: &GuildMemberUpdateEvent) -> GatewayMember {
    GatewayMember::new(
        event.guild_id.get(),
        event.user.id.get(),
        event.nick.clone(),
    )
}

async fn run_command_handler(
    handler: Arc<dyn InteractionHandler>,
    request: InteractionRequest,
    qualified_name: Option<&str>,
    responder: Arc<dyn InteractionResponder>,
    hooks: Option<&GlobalInteractionHooks>,
) {
    if let Err(handler_error) = handler
        .handle(request.clone(), Arc::clone(&responder))
        .await
    {
        if let Some(hooks) = hooks {
            hooks
                .handle_error(
                    &request,
                    qualified_name,
                    &handler_error,
                    Arc::clone(&responder),
                )
                .await;
            return;
        }
        error!(%handler_error, "interaction handler failed without global policy");
        if let Err(error) = report_handler_failure(&responder).await {
            error!(%error, "failed to report interaction handler failure");
        }
    }
}

async fn run_component_handler(
    handler: Arc<dyn InteractionHandler>,
    request: InteractionRequest,
    responder: Arc<dyn InteractionResponder>,
) {
    run_component_handler_with_deadline(
        handler,
        request,
        responder,
        COMPONENT_ACKNOWLEDGEMENT_DEADLINE,
    )
    .await;
}

async fn run_component_handler_with_deadline(
    handler: Arc<dyn InteractionHandler>,
    request: InteractionRequest,
    responder: Arc<dyn InteractionResponder>,
    acknowledgement_deadline: Duration,
) {
    let acknowledgement_policy = handler.acknowledgement_policy(&request);
    let handler_future = handler.handle(request, Arc::clone(&responder));
    tokio::pin!(handler_future);

    let result = if acknowledgement_policy == InteractionAcknowledgementPolicy::Automatic {
        tokio::select! {
            result = &mut handler_future => result,
            () = tokio::time::sleep(acknowledgement_deadline) => {
                if responder.initial_response_state()
                    == InteractionInitialResponseState::NotStarted
                    && let Err(error) = responder.defer(false).await
                {
                    warn!(%error, "automatic component acknowledgement failed");
                }
                handler_future.await
            }
        }
    } else {
        handler_future.await
    };

    match result {
        Err(handler_error) => {
            error!(%handler_error, "component interaction handler failed");
            if let Err(error) = report_handler_failure(&responder).await {
                error!(%error, "failed to report component interaction handler failure");
            }
        }
        Ok(())
            if responder.initial_response_state()
                == InteractionInitialResponseState::NotStarted =>
        {
            if let Err(error) = responder
                .respond(
                    InteractionResponse::message("This control is no longer active.").ephemeral(),
                )
                .await
            {
                warn!(%error, "failed to acknowledge inactive component control");
            }
        }
        Ok(()) => {}
    }
}

/// Deliver the generic failure copy through whichever channel the interaction
/// can still accept: the initial response when the handler failed before
/// acknowledging, a followup otherwise. Discord rejects a followup on an
/// unacknowledged interaction, which previously left the user with only the
/// client-side "This interaction failed" notice.
async fn report_handler_failure(
    responder: &Arc<dyn InteractionResponder>,
) -> Result<(), InteractionResponseError> {
    let message = InteractionResponse::message("Something went wrong.").ephemeral();
    match responder.initial_response_state() {
        InteractionInitialResponseState::NotStarted => responder.respond(message).await,
        InteractionInitialResponseState::Accepted => responder.followup(message).await,
        InteractionInitialResponseState::Attempting | InteractionInitialResponseState::Rejected => {
            Ok(())
        }
    }
}

fn create_command(spec: &CommandSpec) -> CreateCommand {
    CreateCommand::new(&spec.name)
        .description(&spec.description)
        .set_options(spec.options.iter().map(create_option).collect())
}

fn create_option(spec: &CommandOptionSpec) -> CreateCommandOption {
    let mut option =
        CreateCommandOption::new(option_kind(spec.kind), &spec.name, &spec.description)
            .required(spec.required)
            .set_sub_options(spec.options.iter().map(create_option));
    for choice in &spec.choices {
        option = match choice {
            CommandOptionChoice::String { name, value } => option.add_string_choice(name, value),
            CommandOptionChoice::Integer { name, value } => {
                option.add_int_choice(name, i32::try_from(*value).unwrap_or_default())
            }
            CommandOptionChoice::Number { name, value } => option.add_number_choice(name, *value),
        };
    }
    if let Some(value) = spec.min_integer.and_then(|value| u64::try_from(value).ok()) {
        option = option.min_int_value(value);
    }
    if let Some(value) = spec.max_integer.and_then(|value| u64::try_from(value).ok()) {
        option = option.max_int_value(value);
    }
    if let Some(value) = spec.min_number {
        option = option.min_number_value(value);
    }
    if let Some(value) = spec.max_number {
        option = option.max_number_value(value);
    }
    if let Some(value) = spec.min_length {
        option = option.min_length(value);
    }
    if let Some(value) = spec.max_length {
        option = option.max_length(value);
    }
    option = option.set_autocomplete(spec.autocomplete);
    option
}

const fn option_kind(kind: CommandOptionKind) -> CommandOptionType {
    match kind {
        CommandOptionKind::Subcommand => CommandOptionType::SubCommand,
        CommandOptionKind::SubcommandGroup => CommandOptionType::SubCommandGroup,
        CommandOptionKind::String => CommandOptionType::String,
        CommandOptionKind::Integer => CommandOptionType::Integer,
        CommandOptionKind::Boolean => CommandOptionType::Boolean,
        CommandOptionKind::User => CommandOptionType::User,
        CommandOptionKind::Channel => CommandOptionType::Channel,
        CommandOptionKind::Role => CommandOptionType::Role,
        CommandOptionKind::Mentionable => CommandOptionType::Mentionable,
        CommandOptionKind::Number => CommandOptionType::Number,
        CommandOptionKind::Attachment => CommandOptionType::Attachment,
    }
}

fn interaction_option(
    option: &CommandDataOption,
    command: &CommandInteraction,
) -> InteractionOption {
    let value = match &option.value {
        CommandDataOptionValue::SubCommand(options) => InteractionValue::Subcommand(
            options
                .iter()
                .map(|option| interaction_option(option, command))
                .collect(),
        ),
        CommandDataOptionValue::SubCommandGroup(options) => InteractionValue::SubcommandGroup(
            options
                .iter()
                .map(|option| interaction_option(option, command))
                .collect(),
        ),
        CommandDataOptionValue::String(value)
        | CommandDataOptionValue::Autocomplete { value, .. } => {
            InteractionValue::String(value.clone())
        }
        CommandDataOptionValue::Integer(value) => InteractionValue::Integer(*value),
        CommandDataOptionValue::Boolean(value) => InteractionValue::Boolean(*value),
        CommandDataOptionValue::User(value) => InteractionValue::User {
            id: value.get(),
            is_bot: command.data.resolved.users.get(value).map(|user| user.bot),
            display_name: command.data.resolved.members.get(value).map_or_else(
                || {
                    command
                        .data
                        .resolved
                        .users
                        .get(value)
                        .map(|user| user.display_name().to_owned())
                },
                |member| {
                    member.nick.clone().or_else(|| {
                        command
                            .data
                            .resolved
                            .users
                            .get(value)
                            .map(|user| user.display_name().to_owned())
                    })
                },
            ),
        },
        CommandDataOptionValue::Channel(value) => InteractionValue::Channel(value.get()),
        CommandDataOptionValue::Role(value) => InteractionValue::Role(value.get()),
        CommandDataOptionValue::Mentionable(value) => InteractionValue::Mentionable(value.get()),
        CommandDataOptionValue::Number(value) => InteractionValue::Number(*value),
        CommandDataOptionValue::Attachment(id) => command.data.resolved.attachments.get(id).map_or(
            InteractionValue::Unknown,
            |attachment| InteractionValue::Attachment {
                id: id.get(),
                filename: attachment.filename.clone(),
                url: attachment.url.clone(),
                size: u64::from(attachment.size),
            },
        ),
        CommandDataOptionValue::Unknown(_) => InteractionValue::Unknown,
        _ => InteractionValue::Unknown,
    };
    InteractionOption {
        name: option.name.clone(),
        value,
    }
}

fn component_values(kind: &ComponentInteractionDataKind) -> Vec<String> {
    match kind {
        ComponentInteractionDataKind::StringSelect { values } => values.clone(),
        ComponentInteractionDataKind::UserSelect { values } => {
            values.iter().map(ToString::to_string).collect()
        }
        ComponentInteractionDataKind::RoleSelect { values } => {
            values.iter().map(ToString::to_string).collect()
        }
        ComponentInteractionDataKind::MentionableSelect { values } => {
            values.iter().map(ToString::to_string).collect()
        }
        ComponentInteractionDataKind::ChannelSelect { values } => {
            values.iter().map(ToString::to_string).collect()
        }
        ComponentInteractionDataKind::Button | ComponentInteractionDataKind::Unknown(_) => {
            Vec::new()
        }
    }
}

struct CommandResponder {
    interaction: CommandInteraction,
    http: Arc<Http>,
    acknowledged: AtomicBool,
}

impl CommandResponder {
    async fn send_followup(
        &self,
        response: InteractionResponse,
    ) -> Result<InteractionMessageReceipt, InteractionResponseError> {
        let fallback = response.clone();
        match self
            .interaction
            .create_followup(&self.http, followup_response(response))
            .await
        {
            Ok(message) => Ok(InteractionMessageReceipt {
                message_id: message.id.get(),
                channel_id: message.channel_id.get(),
                delivery: InteractionMessageDelivery::InteractionFollowup,
            }),
            Err(error) if fallback.ephemeral => Err(response_error(error)),
            Err(error) => {
                warn!(%error, "interaction follow-up failed; sending directly to the channel");
                self.interaction
                    .channel_id
                    .send_message(&self.http, channel_response(fallback))
                    .await
                    .map(|message| InteractionMessageReceipt {
                        message_id: message.id.get(),
                        channel_id: message.channel_id.get(),
                        delivery: InteractionMessageDelivery::ChannelFallback,
                    })
                    .map_err(response_error)
            }
        }
    }
}

#[async_trait]
impl InteractionResponder for CommandResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        let result = self
            .interaction
            .create_response(&self.http, initial_response(response))
            .await
            .map_err(response_error);
        if result.is_ok() {
            self.acknowledged.store(true, Ordering::Release);
        }
        result
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        let result = self
            .interaction
            .create_response(
                &self.http,
                CreateInteractionResponse::Defer(
                    CreateInteractionResponseMessage::new().ephemeral(ephemeral),
                ),
            )
            .await
            .map_err(response_error);
        if result.is_ok() {
            self.acknowledged.store(true, Ordering::Release);
        }
        result
    }

    async fn autocomplete(
        &self,
        choices: Vec<CommandOptionChoice>,
    ) -> Result<(), InteractionResponseError> {
        let response = choices.into_iter().take(25).fold(
            CreateAutocompleteResponse::new(),
            |response, choice| match choice {
                CommandOptionChoice::String { name, value } => {
                    response.add_string_choice(name, value)
                }
                CommandOptionChoice::Integer { name, value } => {
                    response.add_int_choice(name, value)
                }
                CommandOptionChoice::Number { name, value } => {
                    response.add_number_choice(name, value)
                }
            },
        );
        let result = self
            .interaction
            .create_response(
                &self.http,
                CreateInteractionResponse::Autocomplete(response),
            )
            .await
            .map_err(response_error);
        if result.is_ok() {
            self.acknowledged.store(true, Ordering::Release);
        }
        result
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.send_followup(response).await.map(drop)
    }

    fn response_is_done(&self) -> bool {
        self.acknowledged.load(Ordering::Acquire)
    }

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        self.send_followup(response).await.map(Some)
    }

    async fn edit_original(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        let result = if is_component_only_edit(&response) {
            edit_component_only_original(&self.http, &self.interaction.token, response).await
        } else {
            self.interaction
                .edit_response(&self.http, edit_response(response))
                .await
        };
        result.map(drop).map_err(response_error)
    }

    async fn edit_message(
        &self,
        receipt: InteractionMessageReceipt,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        match receipt.delivery {
            InteractionMessageDelivery::InteractionFollowup => {
                let message_id = MessageId::new(receipt.message_id);
                if is_component_only_edit(&response) {
                    edit_component_only_followup(
                        &self.http,
                        &self.interaction.token,
                        message_id,
                        response,
                    )
                    .await
                    .map(drop)
                    .map_err(response_error)
                } else if preserve_empty_attachments(&response) {
                    let payload = attachment_preserving_message(response);
                    self.http
                        .edit_followup_message(
                            &self.interaction.token,
                            message_id,
                            &payload,
                            Vec::new(),
                        )
                        .await
                        .map(drop)
                        .map_err(response_error)
                } else {
                    self.interaction
                        .edit_followup(&self.http, message_id, edit_followup_message(response))
                        .await
                        .map(drop)
                        .map_err(response_error)
                }
            }
            InteractionMessageDelivery::ChannelFallback => ChannelId::new(receipt.channel_id)
                .edit_message(
                    &self.http,
                    MessageId::new(receipt.message_id),
                    edit_receipt_channel_message(response),
                )
                .await
                .map(drop)
                .map_err(response_error),
        }
    }

    async fn delete_message(
        &self,
        receipt: InteractionMessageReceipt,
    ) -> Result<(), InteractionResponseError> {
        match receipt.delivery {
            InteractionMessageDelivery::InteractionFollowup => self
                .interaction
                .delete_followup(&self.http, MessageId::new(receipt.message_id))
                .await
                .map_err(response_error),
            InteractionMessageDelivery::ChannelFallback => ChannelId::new(receipt.channel_id)
                .delete_message(&self.http, MessageId::new(receipt.message_id))
                .await
                .map_err(response_error),
        }
    }
}

const INITIAL_RESPONSE_NOT_STARTED: u8 = 0;
const INITIAL_RESPONSE_ATTEMPTING: u8 = 1;
const INITIAL_RESPONSE_ACCEPTED: u8 = 2;
const INITIAL_RESPONSE_REJECTED: u8 = 3;

enum InitialResponseClaim {
    Send,
    AlreadyAccepted,
}

/// Coordinates the single initial callback shared by the handler and the
/// acknowledgement watchdog.
///
/// This is the Rust equivalent of checking `interaction.response.is_done()`
/// around discord.py's safe defer/respond helpers, with an explicit in-flight
/// state so two concurrent callbacks can never be sent.
struct CoordinatedResponder {
    inner: Arc<dyn InteractionResponder>,
    initial_state: AtomicU8,
    initial_settled: Notify,
}

fn coordinated_responder(inner: Arc<dyn InteractionResponder>) -> Arc<dyn InteractionResponder> {
    Arc::new(CoordinatedResponder {
        inner,
        initial_state: AtomicU8::new(INITIAL_RESPONSE_NOT_STARTED),
        initial_settled: Notify::new(),
    })
}

impl CoordinatedResponder {
    fn state(&self) -> InteractionInitialResponseState {
        match self.initial_state.load(Ordering::Acquire) {
            INITIAL_RESPONSE_ATTEMPTING => InteractionInitialResponseState::Attempting,
            INITIAL_RESPONSE_ACCEPTED => InteractionInitialResponseState::Accepted,
            INITIAL_RESPONSE_REJECTED => InteractionInitialResponseState::Rejected,
            _ => InteractionInitialResponseState::NotStarted,
        }
    }

    async fn claim_initial(&self) -> Result<InitialResponseClaim, InteractionResponseError> {
        loop {
            let settled = self.initial_settled.notified();
            tokio::pin!(settled);
            settled.as_mut().enable();
            match self.initial_state.load(Ordering::Acquire) {
                INITIAL_RESPONSE_NOT_STARTED => {
                    if self
                        .initial_state
                        .compare_exchange(
                            INITIAL_RESPONSE_NOT_STARTED,
                            INITIAL_RESPONSE_ATTEMPTING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return Ok(InitialResponseClaim::Send);
                    }
                }
                INITIAL_RESPONSE_ATTEMPTING => settled.await,
                INITIAL_RESPONSE_ACCEPTED => {
                    return Ok(InitialResponseClaim::AlreadyAccepted);
                }
                INITIAL_RESPONSE_REJECTED => {
                    return Err(InteractionResponseError::new(
                        "the initial interaction response was already rejected",
                    ));
                }
                _ => unreachable!("invalid interaction response state"),
            }
        }
    }

    fn finish_initial(
        &self,
        result: Result<(), InteractionResponseError>,
    ) -> Result<(), InteractionResponseError> {
        self.initial_state.store(
            if result.is_ok() {
                INITIAL_RESPONSE_ACCEPTED
            } else {
                INITIAL_RESPONSE_REJECTED
            },
            Ordering::Release,
        );
        self.initial_settled.notify_waiters();
        result
    }
}

#[async_trait]
impl InteractionResponder for CoordinatedResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        match self.claim_initial().await? {
            InitialResponseClaim::Send => {
                let result = self.inner.respond(response).await;
                self.finish_initial(result)
            }
            InitialResponseClaim::AlreadyAccepted => self.inner.followup(response).await,
        }
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        match self.claim_initial().await? {
            InitialResponseClaim::Send => {
                let result = self.inner.defer(ephemeral).await;
                self.finish_initial(result)
            }
            InitialResponseClaim::AlreadyAccepted => Ok(()),
        }
    }

    async fn defer_thinking(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        match self.claim_initial().await? {
            InitialResponseClaim::Send => {
                let result = self.inner.defer_thinking(ephemeral).await;
                self.finish_initial(result)
            }
            InitialResponseClaim::AlreadyAccepted => Ok(()),
        }
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.inner.followup(response).await
    }

    async fn show_modal(&self, modal: InteractionModal) -> Result<(), InteractionResponseError> {
        match self.claim_initial().await? {
            InitialResponseClaim::Send => {
                let result = self.inner.show_modal(modal).await;
                self.finish_initial(result)
            }
            InitialResponseClaim::AlreadyAccepted => Err(InteractionResponseError::new(
                "cannot open a modal after acknowledging the interaction",
            )),
        }
    }

    async fn autocomplete(
        &self,
        choices: Vec<CommandOptionChoice>,
    ) -> Result<(), InteractionResponseError> {
        match self.claim_initial().await? {
            InitialResponseClaim::Send => {
                let result = self.inner.autocomplete(choices).await;
                self.finish_initial(result)
            }
            InitialResponseClaim::AlreadyAccepted => Err(InteractionResponseError::new(
                "autocomplete interaction was already acknowledged",
            )),
        }
    }

    fn response_is_done(&self) -> bool {
        self.state() != InteractionInitialResponseState::NotStarted
    }

    fn initial_response_state(&self) -> InteractionInitialResponseState {
        self.state()
    }

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        self.inner.followup_with_receipt(response).await
    }

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        match self.claim_initial().await? {
            InitialResponseClaim::Send => {
                let result = self.inner.update(response).await;
                self.finish_initial(result)
            }
            InitialResponseClaim::AlreadyAccepted => self.inner.edit_original(response).await,
        }
    }

    async fn edit_original(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.inner.edit_original(response).await
    }

    async fn edit_message(
        &self,
        receipt: InteractionMessageReceipt,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.inner.edit_message(receipt, response).await
    }

    async fn delete_message(
        &self,
        receipt: InteractionMessageReceipt,
    ) -> Result<(), InteractionResponseError> {
        self.inner.delete_message(receipt).await
    }
}

struct ComponentResponder {
    interaction: ComponentInteraction,
    http: Arc<Http>,
    acknowledged: AtomicBool,
}

impl ComponentResponder {
    async fn send_followup(
        &self,
        response: InteractionResponse,
    ) -> Result<InteractionMessageReceipt, InteractionResponseError> {
        let fallback = response.clone();
        match self
            .interaction
            .create_followup(&self.http, followup_response(response))
            .await
        {
            Ok(message) => Ok(InteractionMessageReceipt {
                message_id: message.id.get(),
                channel_id: message.channel_id.get(),
                delivery: InteractionMessageDelivery::InteractionFollowup,
            }),
            Err(error) if fallback.ephemeral => Err(response_error(error)),
            Err(error) => {
                warn!(%error, "component follow-up failed; sending directly to the channel");
                self.interaction
                    .channel_id
                    .send_message(&self.http, channel_response(fallback))
                    .await
                    .map(|message| InteractionMessageReceipt {
                        message_id: message.id.get(),
                        channel_id: message.channel_id.get(),
                        delivery: InteractionMessageDelivery::ChannelFallback,
                    })
                    .map_err(response_error)
            }
        }
    }
}

#[async_trait]
impl InteractionResponder for ComponentResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        let result = self
            .interaction
            .create_response(&self.http, initial_response(response))
            .await
            .map_err(response_error);
        if result.is_ok() {
            self.acknowledged.store(true, Ordering::Release);
        }
        result
    }

    async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
        let result = self
            .interaction
            .create_response(&self.http, component_defer_response(false, false))
            .await
            .map_err(response_error);
        if result.is_ok() {
            self.acknowledged.store(true, Ordering::Release);
        }
        result
    }

    async fn defer_thinking(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        let result = self
            .interaction
            .create_response(&self.http, component_defer_response(true, ephemeral))
            .await
            .map_err(response_error);
        if result.is_ok() {
            self.acknowledged.store(true, Ordering::Release);
        }
        result
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.send_followup(response).await.map(drop)
    }

    async fn show_modal(&self, modal: InteractionModal) -> Result<(), InteractionResponseError> {
        let result = self
            .interaction
            .create_response(&self.http, serenity_modal(modal))
            .await
            .map_err(response_error);
        if result.is_ok() {
            self.acknowledged.store(true, Ordering::Release);
        }
        result
    }

    fn response_is_done(&self) -> bool {
        self.acknowledged.load(Ordering::Acquire)
    }

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        self.send_followup(response).await.map(Some)
    }

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        let result =
            if preserve_empty_attachments(&response) || is_component_only_edit(&response) {
                let payload = attachment_preserving_interaction_callback(response);
                self.http
                    .create_interaction_response(
                        self.interaction.id,
                        &self.interaction.token,
                        &payload,
                        Vec::new(),
                    )
                    .await
            } else {
                self.interaction
                    .create_response(
                        &self.http,
                        CreateInteractionResponse::UpdateMessage(response_message(response)),
                    )
                    .await
            }
            .map_err(response_error);
        if result.is_ok() {
            self.acknowledged.store(true, Ordering::Release);
        }
        result
    }

    async fn edit_original(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        let result = if is_component_only_edit(&response) {
            edit_component_only_original(&self.http, &self.interaction.token, response).await
        } else {
            self.interaction
                .edit_response(&self.http, edit_response(response))
                .await
        };
        result.map(drop).map_err(response_error)
    }

    async fn edit_message(
        &self,
        receipt: InteractionMessageReceipt,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        match receipt.delivery {
            InteractionMessageDelivery::InteractionFollowup => {
                let message_id = MessageId::new(receipt.message_id);
                if is_component_only_edit(&response) {
                    edit_component_only_followup(
                        &self.http,
                        &self.interaction.token,
                        message_id,
                        response,
                    )
                    .await
                    .map(drop)
                    .map_err(response_error)
                } else if preserve_empty_attachments(&response) {
                    let payload = attachment_preserving_message(response);
                    self.http
                        .edit_followup_message(
                            &self.interaction.token,
                            message_id,
                            &payload,
                            Vec::new(),
                        )
                        .await
                        .map(drop)
                        .map_err(response_error)
                } else {
                    self.interaction
                        .edit_followup(&self.http, message_id, edit_followup_message(response))
                        .await
                        .map(drop)
                        .map_err(response_error)
                }
            }
            InteractionMessageDelivery::ChannelFallback => ChannelId::new(receipt.channel_id)
                .edit_message(
                    &self.http,
                    MessageId::new(receipt.message_id),
                    edit_receipt_channel_message(response),
                )
                .await
                .map(drop)
                .map_err(response_error),
        }
    }

    async fn delete_message(
        &self,
        receipt: InteractionMessageReceipt,
    ) -> Result<(), InteractionResponseError> {
        match receipt.delivery {
            InteractionMessageDelivery::InteractionFollowup => self
                .interaction
                .delete_followup(&self.http, MessageId::new(receipt.message_id))
                .await
                .map_err(response_error),
            InteractionMessageDelivery::ChannelFallback => ChannelId::new(receipt.channel_id)
                .delete_message(&self.http, MessageId::new(receipt.message_id))
                .await
                .map_err(response_error),
        }
    }
}

fn component_defer_response(thinking: bool, ephemeral: bool) -> CreateInteractionResponse {
    if thinking {
        CreateInteractionResponse::Defer(
            CreateInteractionResponseMessage::new().ephemeral(ephemeral),
        )
    } else {
        CreateInteractionResponse::Acknowledge
    }
}

struct ModalResponder {
    interaction: ModalInteraction,
    http: Arc<Http>,
    acknowledged: AtomicBool,
}

#[async_trait]
impl InteractionResponder for ModalResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        let result = self
            .interaction
            .create_response(&self.http, initial_response(response))
            .await
            .map_err(response_error);
        if result.is_ok() {
            self.acknowledged.store(true, Ordering::Release);
        }
        result
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        let response = if ephemeral {
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            )
        } else {
            // A modal opened from a persistent message uses a deferred message
            // update, matching discord.py's `defer(thinking=False)`.
            CreateInteractionResponse::Acknowledge
        };
        let result = self
            .interaction
            .create_response(&self.http, response)
            .await
            .map_err(response_error);
        if result.is_ok() {
            self.acknowledged.store(true, Ordering::Release);
        }
        result
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.interaction
            .create_followup(&self.http, followup_response(response))
            .await
            .map(drop)
            .map_err(response_error)
    }

    fn response_is_done(&self) -> bool {
        self.acknowledged.load(Ordering::Acquire)
    }

    async fn edit_original(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        let result = if is_component_only_edit(&response) {
            edit_component_only_original(&self.http, &self.interaction.token, response).await
        } else {
            self.interaction
                .edit_response(&self.http, edit_response(response))
                .await
        };
        result.map(drop).map_err(response_error)
    }
}

fn serenity_modal(modal: InteractionModal) -> CreateInteractionResponse {
    let rows = modal
        .inputs
        .into_iter()
        .map(|input| {
            let style = match input.style {
                InteractionTextInputStyle::Short => InputTextStyle::Short,
                InteractionTextInputStyle::Paragraph => InputTextStyle::Paragraph,
            };
            let mut builder =
                CreateInputText::new(style, input.label, input.custom_id).required(input.required);
            if let Some(placeholder) = input.placeholder {
                builder = builder.placeholder(placeholder);
            }
            if let Some(min_length) = input.min_length {
                builder = builder.min_length(min_length);
            }
            if let Some(max_length) = input.max_length {
                builder = builder.max_length(max_length);
            }
            if let Some(value) = input.value {
                builder = builder.value(value);
            }
            CreateActionRow::InputText(builder)
        })
        .collect();
    CreateInteractionResponse::Modal(
        CreateModal::new(modal.custom_id, modal.title).components(rows),
    )
}

fn initial_response(response: InteractionResponse) -> CreateInteractionResponse {
    CreateInteractionResponse::Message(response_message(response))
}

fn response_message(response: InteractionResponse) -> CreateInteractionResponseMessage {
    let component_only = is_component_only_edit(&response);
    let include_attachments = include_response_attachments(&response);
    let mut message = CreateInteractionResponseMessage::new().ephemeral(response.ephemeral);
    if !response.content.is_empty() {
        message = message.content(response.content);
    }
    if let Some(allowed_mentions) = serenity_allowed_mentions(&response.allowed_mentions) {
        message = message.allowed_mentions(allowed_mentions);
    }
    if !component_only {
        message = message.embeds(response.embeds.iter().map(serenity_embed).collect());
        if include_attachments {
            message = message.files(response.attachments.iter().map(serenity_attachment));
        }
    }
    message.components(serenity_components(&response.components))
}

fn followup_response(response: InteractionResponse) -> CreateInteractionResponseFollowup {
    let include_attachments = include_response_attachments(&response);
    let mut message = CreateInteractionResponseFollowup::new().ephemeral(response.ephemeral);
    if !response.content.is_empty() {
        message = message.content(response.content);
    }
    if let Some(allowed_mentions) = serenity_allowed_mentions(&response.allowed_mentions) {
        message = message.allowed_mentions(allowed_mentions);
    }
    message = message.embeds(response.embeds.iter().map(serenity_embed).collect());
    if include_attachments {
        message = message.files(response.attachments.iter().map(serenity_attachment));
    }
    message.components(serenity_components(&response.components))
}

#[derive(serde::Serialize)]
struct AttachmentPreservingInteractionMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    embeds: Option<Vec<CreateEmbed>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_mentions: Option<CreateAllowedMentions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    components: Option<Vec<CreateActionRow>>,
}

#[derive(serde::Serialize)]
struct AttachmentPreservingInteractionCallback {
    #[serde(rename = "type")]
    response_type: u8,
    data: AttachmentPreservingInteractionMessage,
}

fn attachment_preserving_message(
    response: InteractionResponse,
) -> AttachmentPreservingInteractionMessage {
    let component_only = is_component_only_edit(&response);
    AttachmentPreservingInteractionMessage {
        content: (!response.content.is_empty()).then_some(response.content),
        embeds: (!component_only).then_some(response.embeds.iter().map(serenity_embed).collect()),
        allowed_mentions: serenity_allowed_mentions(&response.allowed_mentions),
        components: Some(serenity_components(&response.components)),
    }
}

fn attachment_preserving_interaction_callback(
    response: InteractionResponse,
) -> AttachmentPreservingInteractionCallback {
    AttachmentPreservingInteractionCallback {
        response_type: 7,
        data: attachment_preserving_message(response),
    }
}

fn edit_followup_message(response: InteractionResponse) -> CreateInteractionResponseFollowup {
    if is_component_only_edit(&response) {
        let mut message = CreateInteractionResponseFollowup::new()
            .components(serenity_components(&response.components));
        if let Some(allowed_mentions) = serenity_allowed_mentions(&response.allowed_mentions) {
            message = message.allowed_mentions(allowed_mentions);
        }
        message
    } else {
        followup_response(response)
    }
}

/// Serenity 0.12.5's `CreateInteractionResponseFollowup` always serializes
/// its `attachments` field, even when no files were supplied. Discord treats
/// that empty list as an instruction to remove existing uploads on an edit.
/// Component-only edits must therefore use the lower-level HTTP adapter with
/// a payload that omits the field entirely, matching Discord.py's view-only
/// edit behavior.
#[derive(serde::Serialize)]
struct ComponentOnlyInteractionEdit {
    components: Vec<CreateActionRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_mentions: Option<CreateAllowedMentions>,
}

fn component_only_interaction_edit(response: InteractionResponse) -> ComponentOnlyInteractionEdit {
    ComponentOnlyInteractionEdit {
        components: serenity_components(&response.components),
        allowed_mentions: serenity_allowed_mentions(&response.allowed_mentions),
    }
}

async fn edit_component_only_original(
    http: &Http,
    interaction_token: &str,
    response: InteractionResponse,
) -> serenity::Result<serenity::all::Message> {
    let edit = component_only_interaction_edit(response);
    http.edit_original_interaction_response(interaction_token, &edit, Vec::new())
        .await
}

/// Resolve a guild's name over HTTP when the gateway cache cannot answer.
///
/// Split out from [`SerenityDiscordTransport::guild_name`] so the fallback is
/// reachable in tests: the method itself needs a live gateway context first,
/// which is why its siblings have no coverage.
async fn fetch_guild_name(http: &Http, guild_id: GuildId) -> Result<Option<String>, String> {
    match guild_id.to_partial_guild(http).await {
        Ok(guild) => Ok(Some(guild.name)),
        Err(serenity::Error::Http(error))
            if error
                .status_code()
                .is_some_and(|status| status.as_u16() == 404) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.to_string()),
    }
}

async fn edit_component_only_followup(
    http: &Http,
    interaction_token: &str,
    message_id: MessageId,
    response: InteractionResponse,
) -> serenity::Result<serenity::all::Message> {
    let edit = component_only_interaction_edit(response);
    http.edit_followup_message(interaction_token, message_id, &edit, Vec::new())
        .await
}

fn edit_receipt_channel_message(response: InteractionResponse) -> EditMessage {
    let component_only = is_component_only_edit(&response);
    let preserve_content = response.content.is_empty() && preserve_empty_attachments(&response);
    let message = DiscordMessage::default_mentions(response);
    edit_channel_response(if component_only {
        message.preserving_body()
    } else if preserve_content {
        message.preserving_content()
    } else {
        message
    })
}

fn is_component_only_edit(response: &InteractionResponse) -> bool {
    response.attachment_policy != InteractionAttachmentPolicy::Clear
        && response.content.is_empty()
        && response.embeds.is_empty()
        && response.attachments.is_empty()
        && !response.components.is_empty()
}

fn channel_response(response: InteractionResponse) -> CreateMessage {
    let include_attachments = include_response_attachments(&response);
    let mut message = CreateMessage::new();
    if !response.content.is_empty() {
        message = message.content(response.content);
    }
    if let Some(allowed_mentions) = serenity_allowed_mentions(&response.allowed_mentions) {
        message = message.allowed_mentions(allowed_mentions);
    }
    message = message.embeds(response.embeds.iter().map(serenity_embed).collect());
    if include_attachments {
        message = message.files(response.attachments.iter().map(serenity_attachment));
    }
    message.components(serenity_components(&response.components))
}

fn edit_response(response: InteractionResponse) -> EditInteractionResponse {
    let include_attachments = include_response_attachments(&response);
    let mut edit = EditInteractionResponse::new()
        .content(response.content)
        .embeds(response.embeds.iter().map(serenity_embed).collect())
        .components(serenity_components(&response.components));
    if include_attachments {
        let attachments = response
            .attachments
            .iter()
            .map(serenity_attachment)
            .fold(EditAttachments::new(), EditAttachments::add);
        edit = edit.attachments(attachments);
    }
    if let Some(allowed_mentions) = serenity_allowed_mentions(&response.allowed_mentions) {
        edit = edit.allowed_mentions(allowed_mentions);
    }
    edit
}

fn serenity_allowed_mentions(policy: &InteractionAllowedMentions) -> Option<CreateAllowedMentions> {
    match policy {
        InteractionAllowedMentions::Default => None,
        InteractionAllowedMentions::None => Some(CreateAllowedMentions::new()),
        InteractionAllowedMentions::Users(user_ids) => {
            Some(CreateAllowedMentions::new().users(user_ids.iter().copied()))
        }
    }
}

fn include_response_attachments(response: &InteractionResponse) -> bool {
    !response.attachments.is_empty()
        || response.attachment_policy != InteractionAttachmentPolicy::Preserve
}

fn preserve_empty_attachments(response: &InteractionResponse) -> bool {
    response.attachments.is_empty()
        && response.attachment_policy == InteractionAttachmentPolicy::Preserve
}

fn serenity_embed(embed: &InteractionEmbed) -> CreateEmbed {
    let mut result = CreateEmbed::new();
    if let Some(title) = &embed.title {
        result = result.title(title);
    }
    if let Some(description) = &embed.description {
        result = result.description(description);
    }
    if let Some(color) = embed.color {
        result = result.color(color);
    }
    if let Some(author_name) = &embed.author_name {
        let mut author = CreateEmbedAuthor::new(author_name);
        if let Some(icon_url) = &embed.author_icon_url {
            author = author.icon_url(icon_url);
        }
        result = result.author(author);
    }
    if let Some(image_url) = &embed.image_url {
        result = result.image(image_url);
    }
    if let Some(thumbnail_url) = &embed.thumbnail_url {
        result = result.thumbnail(thumbnail_url);
    }
    if let Some(footer) = &embed.footer {
        let mut rendered_footer = CreateEmbedFooter::new(footer);
        if let Some(icon_url) = &embed.footer_icon_url {
            rendered_footer = rendered_footer.icon_url(icon_url);
        }
        result = result.footer(rendered_footer);
    }
    for field in &embed.fields {
        result = result.field(&field.name, &field.value, field.inline);
    }
    result
}

fn serenity_components(rows: &[InteractionActionRow]) -> Vec<CreateActionRow> {
    rows.iter()
        .map(|row| {
            if let Some(select) = &row.string_select {
                let options = select
                    .options
                    .iter()
                    .map(|option| {
                        let builder = CreateSelectMenuOption::new(&option.label, &option.value);
                        option
                            .description
                            .as_ref()
                            .map_or(builder.clone(), |description| {
                                builder.description(description)
                            })
                    })
                    .collect();
                CreateActionRow::SelectMenu(
                    CreateSelectMenu::new(
                        &select.custom_id,
                        CreateSelectMenuKind::String { options },
                    )
                    .placeholder(&select.placeholder)
                    .min_values(select.min_values)
                    .max_values(select.max_values)
                    .disabled(select.disabled),
                )
            } else {
                CreateActionRow::Buttons(
                    row.buttons
                        .iter()
                        .map(|button| {
                            let mut builder = CreateButton::new(&button.custom_id)
                                .label(&button.label)
                                .style(match button.style {
                                    InteractionButtonStyle::Primary => ButtonStyle::Primary,
                                    InteractionButtonStyle::Secondary => ButtonStyle::Secondary,
                                    InteractionButtonStyle::Success => ButtonStyle::Success,
                                    InteractionButtonStyle::Danger => ButtonStyle::Danger,
                                })
                                .disabled(button.disabled);
                            if let Some(emoji) = &button.emoji {
                                builder = builder.emoji(ReactionType::Unicode(emoji.clone()));
                            }
                            builder
                        })
                        .collect(),
                )
            }
        })
        .collect()
}

fn discord_reaction_type(emoji: &DiscordEmoji) -> ReactionType {
    emoji.id.map_or_else(
        || ReactionType::Unicode(emoji.name.clone()),
        |id| ReactionType::Custom {
            animated: false,
            id: serenity::all::EmojiId::new(id),
            name: Some(emoji.name.clone()),
        },
    )
}

fn edit_channel_response(message: DiscordMessage) -> EditMessage {
    let include_attachments = include_response_attachments(&message.response);
    let response = message.response;
    let mut edit = EditMessage::new().components(serenity_components(&response.components));
    if message.content_mode != crate::discord_transport::DiscordMessageContentMode::PreserveBody {
        edit = edit.embeds(response.embeds.iter().map(serenity_embed).collect());
        if include_attachments {
            let attachments = response
                .attachments
                .iter()
                .map(serenity_attachment)
                .fold(EditAttachments::new(), EditAttachments::add);
            edit = edit.attachments(attachments);
        }
    }
    if message.content_mode == crate::discord_transport::DiscordMessageContentMode::Replace {
        edit = edit.content(response.content);
    }
    let mentions = match message.allowed_mentions {
        crate::discord_transport::DiscordAllowedMentions::Default => None,
        crate::discord_transport::DiscordAllowedMentions::None => {
            Some(CreateAllowedMentions::new())
        }
        crate::discord_transport::DiscordAllowedMentions::Users(users) => {
            Some(CreateAllowedMentions::new().users(users))
        }
    };
    if let Some(mentions) = mentions {
        edit = edit.allowed_mentions(mentions);
    }
    edit
}

fn apply_discord_mentions(
    mut response: InteractionResponse,
    message: &DiscordMessage,
) -> InteractionResponse {
    response.allowed_mentions = match &message.allowed_mentions {
        crate::discord_transport::DiscordAllowedMentions::Default => {
            InteractionAllowedMentions::Default
        }
        crate::discord_transport::DiscordAllowedMentions::None => InteractionAllowedMentions::None,
        crate::discord_transport::DiscordAllowedMentions::Users(users) => {
            InteractionAllowedMentions::Users(users.iter().copied().collect())
        }
    };
    response
}

fn serenity_duel_channel_snapshot(
    context: &SerenityContext,
    channel: &GuildChannel,
) -> Option<DuelChannelSnapshot> {
    let id = i64::try_from(channel.id.get()).ok()?;
    let guild_id = i64::try_from(channel.guild_id.get()).ok()?;
    let kind = match channel.kind {
        ChannelType::Text | ChannelType::News => DuelChannelKind::Text,
        ChannelType::NewsThread | ChannelType::PublicThread | ChannelType::PrivateThread => {
            DuelChannelKind::Thread
        }
        _ => DuelChannelKind::Other,
    };
    let can_send = context.cache.guild(channel.guild_id).is_some_and(|guild| {
        let current_user_id = context.cache.current_user().id;
        let Some(member) = guild.members.get(&current_user_id) else {
            return false;
        };
        let permissions = guild.user_permissions_in(channel, member);
        match kind {
            DuelChannelKind::Text => permissions.send_messages(),
            DuelChannelKind::Thread => permissions.send_messages_in_threads(),
            DuelChannelKind::Other => false,
        }
    });
    Some(DuelChannelSnapshot {
        id,
        guild_id: Some(guild_id),
        name: channel.name.clone(),
        kind,
        can_send,
    })
}

fn dig_channel_snapshot(
    context: &SerenityContext,
    channel: &GuildChannel,
) -> Option<DigChannelSnapshot> {
    let id = i64::try_from(channel.id.get()).ok()?;
    let guild_id = i64::try_from(channel.guild_id.get()).ok()?;
    let is_thread = matches!(
        channel.kind,
        ChannelType::NewsThread | ChannelType::PublicThread | ChannelType::PrivateThread
    );
    let is_text = is_thread || matches!(channel.kind, ChannelType::Text | ChannelType::News);
    let can_send = context.cache.guild(channel.guild_id).is_some_and(|guild| {
        let Some(member) = guild.members.get(&context.cache.current_user().id) else {
            return true;
        };
        let permissions = guild.user_permissions_in(channel, member);
        if is_thread {
            permissions.send_messages_in_threads()
        } else {
            permissions.send_messages()
        }
    });
    Some(DigChannelSnapshot {
        id,
        guild_id: Some(guild_id),
        parent_id: channel
            .parent_id
            .and_then(|parent| i64::try_from(parent.get()).ok()),
        can_send,
        is_text,
    })
}

#[async_trait]
impl DuelDiscordPort for SerenityDiscordTransport {
    async fn guild_snapshot(&self, guild_id: i64) -> Result<Option<DuelGuildSnapshot>, String> {
        let context = self.context()?;
        let Ok(guild_id) = u64::try_from(guild_id) else {
            return Ok(None);
        };
        let guild_id = GuildId::new(guild_id);
        let Some((snapshot_guild_id, channels)) = context.cache.guild(guild_id).map(|guild| {
            (
                guild.id,
                guild.channels.values().cloned().collect::<Vec<_>>(),
            )
        }) else {
            return Ok(None);
        };
        let text_channels = channels
            .iter()
            .filter_map(|channel| serenity_duel_channel_snapshot(&context, channel))
            .filter(|channel| channel.kind == DuelChannelKind::Text)
            .collect();
        Ok(Some(DuelGuildSnapshot {
            id: i64::try_from(snapshot_guild_id.get())
                .map_err(|_| "Discord guild ID exceeds SQLite integer range".to_owned())?,
            text_channels,
        }))
    }

    async fn cached_channel(&self, channel_id: i64) -> Result<Option<DuelChannelSnapshot>, String> {
        let context = self.context()?;
        let Ok(channel_id) = u64::try_from(channel_id) else {
            return Ok(None);
        };
        let channel_id = ChannelId::new(channel_id);
        let channel = context.cache.guilds().into_iter().find_map(|guild_id| {
            context.cache.guild(guild_id).and_then(|guild| {
                guild.channels.get(&channel_id).cloned().or_else(|| {
                    guild
                        .threads
                        .iter()
                        .find(|channel| channel.id == channel_id)
                        .cloned()
                })
            })
        });
        Ok(channel
            .as_ref()
            .and_then(|channel| serenity_duel_channel_snapshot(&context, channel)))
    }

    async fn fetch_channel(&self, channel_id: i64) -> Result<Option<DuelChannelSnapshot>, String> {
        let context = self.context()?;
        let Ok(channel_id) = u64::try_from(channel_id) else {
            return Ok(None);
        };
        match context.http.get_channel(ChannelId::new(channel_id)).await {
            Ok(Channel::Guild(channel)) => Ok(serenity_duel_channel_snapshot(&context, &channel)),
            Ok(Channel::Private(_)) => Ok(None),
            Err(serenity::Error::Http(error))
                if error
                    .status_code()
                    .is_some_and(|status| status.as_u16() == 404) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.to_string()),
            #[allow(unreachable_patterns)]
            Ok(_) => Ok(None),
        }
    }

    async fn send_message(&self, channel_id: i64, message: DiscordMessage) -> Result<(), String> {
        let channel_id = u64::try_from(channel_id)
            .map_err(|_| "Discord channel ID is outside the supported range".to_owned())?;
        DiscordTransport::send_message(self, channel_id, message)
            .await
            .map(|_| ())
    }

    async fn edit_message(
        &self,
        channel_id: i64,
        message_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        let channel_id = u64::try_from(channel_id)
            .map_err(|_| "Discord channel ID is outside the supported range".to_owned())?;
        let message_id = u64::try_from(message_id)
            .map_err(|_| "Discord message ID is outside the supported range".to_owned())?;
        DiscordTransport::edit_message(self, channel_id, message_id, message).await
    }
}

#[async_trait]
impl PetSweepDiscordPort for SerenityDiscordTransport {
    async fn cached_text_channel(
        &self,
        guild_id: i64,
        configured_channel_id: i64,
    ) -> Result<Option<i64>, String> {
        let context = self.context()?;
        let (Ok(guild_id), Ok(channel_id)) = (
            u64::try_from(guild_id),
            u64::try_from(configured_channel_id),
        ) else {
            return Ok(None);
        };
        let channel_id = ChannelId::new(channel_id);
        Ok(context
            .cache
            .guild(GuildId::new(guild_id))
            .and_then(|guild| guild.channels.get(&channel_id).cloned())
            .filter(|channel| channel.kind == ChannelType::Text)
            .and_then(|channel| i64::try_from(channel.id.get()).ok()))
    }

    async fn send_channel(
        &self,
        channel_id: i64,
        message: DiscordMessage,
    ) -> Result<(), PetSweepDeliveryError> {
        let context = self.context().map_err(PetSweepDeliveryError::Transient)?;
        let channel_id = u64::try_from(channel_id).map_err(|_| {
            PetSweepDeliveryError::Transient(
                "Discord pet channel ID is outside the supported range".to_owned(),
            )
        })?;
        let response = apply_discord_mentions(message.response.clone(), &message);
        match ChannelId::new(channel_id)
            .send_message(
                (&context.cache, context.http.as_ref()),
                channel_response(response),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(serenity::Error::Http(error))
                if error
                    .status_code()
                    .is_some_and(|status| status.as_u16() == 403) =>
            {
                Err(PetSweepDeliveryError::Forbidden)
            }
            Err(error) => Err(PetSweepDeliveryError::Transient(error.to_string())),
        }
    }

    async fn send_dm(&self, user_id: i64, message: DiscordMessage) -> Result<(), String> {
        let user_id = u64::try_from(user_id)
            .map_err(|_| "Discord pet owner ID is outside the supported range".to_owned())?;
        DiscordTransport::send_direct_message(self, user_id, message).await
    }
}

#[async_trait]
impl PredictionDiscordPort for SerenityDiscordTransport {
    async fn edit_prediction_market_message(
        &self,
        thread_id: i64,
        message_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        let context = self.context()?;
        let thread_id = u64::try_from(thread_id).map_err(|_| {
            "Discord prediction thread ID is outside the supported range".to_owned()
        })?;
        let message_id = u64::try_from(message_id).map_err(|_| {
            "Discord prediction message ID is outside the supported range".to_owned()
        })?;
        let channel_id = ChannelId::new(thread_id);

        // Match Python's fetch-before-unarchive ordering: a vanished message
        // must not revive an archived market thread for nothing.
        let mut fetched = match channel_id
            .message(
                (&context.cache, context.http.as_ref()),
                MessageId::new(message_id),
            )
            .await
        {
            Ok(message) => message,
            Err(serenity::Error::Http(error))
                if error
                    .status_code()
                    .is_some_and(|status| status.as_u16() == 404) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        };
        ensure_prediction_thread_writable(&context, channel_id).await?;

        let response = apply_discord_mentions(message.response.clone(), &message);
        let attachments = response
            .attachments
            .iter()
            .map(serenity_attachment)
            .fold(EditAttachments::new(), EditAttachments::add);
        let mut edit = EditMessage::new()
            .embeds(response.embeds.iter().map(serenity_embed).collect())
            .attachments(attachments);
        if message.content_mode == crate::discord_transport::DiscordMessageContentMode::Replace {
            edit = edit.content(response.content);
        }
        if let Some(mentions) = serenity_allowed_mentions(&response.allowed_mentions) {
            edit = edit.allowed_mentions(mentions);
        }
        // Omit components entirely: Serenity then retains the live persistent
        // market buttons already present on the message.
        fetched
            .edit((&context.cache, context.http.as_ref()), edit)
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }

    async fn send_prediction_thread_message(
        &self,
        thread_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        let context = self.context()?;
        let thread_id = u64::try_from(thread_id).map_err(|_| {
            "Discord prediction thread ID is outside the supported range".to_owned()
        })?;
        let channel_id = ChannelId::new(thread_id);
        ensure_prediction_thread_writable(&context, channel_id).await?;
        let response = apply_discord_mentions(message.response.clone(), &message);
        channel_id
            .send_message(
                (&context.cache, context.http.as_ref()),
                channel_response(response),
            )
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

fn configured_gamba_location_matches(
    configured_id: Option<u64>,
    channel_id: u64,
    parent_id: Option<u64>,
) -> bool {
    configured_id == Some(channel_id) || configured_id == parent_id
}

#[async_trait]
impl PredictionCommandDiscordPort for SerenityDiscordTransport {
    async fn prediction_channel_is_gamba(
        &self,
        guild_id: i64,
        channel_id: i64,
    ) -> Result<bool, String> {
        let context = self.context()?;
        let guild_id = GuildId::new(
            u64::try_from(guild_id)
                .map_err(|_| "Discord prediction guild ID is outside the supported range")?,
        );
        let channel_id = ChannelId::new(
            u64::try_from(channel_id)
                .map_err(|_| "Discord prediction channel ID is outside the supported range")?,
        );
        let configured_id = self
            .gamba_channel_id
            .and_then(|channel_id| u64::try_from(channel_id).ok());
        if configured_gamba_location_matches(configured_id, channel_id.get(), None) {
            return Ok(true);
        }
        let Some(channel) = prediction_channel(&context, guild_id, channel_id).await? else {
            return Ok(false);
        };
        if channel.guild_id != guild_id {
            return Ok(false);
        }
        if channel.name.to_ascii_lowercase().contains("gamba") {
            return Ok(true);
        }
        let Some(parent_id) = channel.parent_id else {
            return Ok(false);
        };
        if configured_gamba_location_matches(configured_id, channel_id.get(), Some(parent_id.get()))
        {
            return Ok(true);
        }
        Ok(prediction_channel(&context, guild_id, parent_id)
            .await?
            .is_some_and(|parent| parent.name.to_ascii_lowercase().contains("gamba")))
    }

    async fn prediction_display_text(&self, guild_id: i64, text: &str) -> Result<String, String> {
        let context = self.context()?;
        let guild_id = GuildId::new(
            u64::try_from(guild_id)
                .map_err(|_| "Discord prediction guild ID is outside the supported range")?,
        );
        Ok(resolve_and_neutralize_discord_mentions(
            text,
            |user_id| {
                let user_id = UserId::new(u64::try_from(user_id).ok()?);
                context
                    .cache
                    .guild(guild_id)
                    .and_then(|guild| {
                        guild
                            .members
                            .get(&user_id)
                            .map(|member| member.display_name().to_owned())
                    })
                    .or_else(|| {
                        context
                            .cache
                            .user(user_id)
                            .map(|user| user.display_name().to_owned())
                    })
                    .or_else(|| Some("unknown".to_owned()))
            },
            |role_id| {
                let role_id = serenity::all::RoleId::new(u64::try_from(role_id).ok()?);
                context
                    .cache
                    .guild(guild_id)
                    .and_then(|guild| guild.roles.get(&role_id).map(|role| role.name.clone()))
                    .or_else(|| Some("unknown-role".to_owned()))
            },
        ))
    }

    fn prediction_display_name(&self, guild_id: i64, user_id: i64) -> String {
        let fallback = format!("<@{user_id}>");
        let (Ok(context), Ok(guild_id), Ok(user_id)) = (
            self.context(),
            u64::try_from(guild_id),
            u64::try_from(user_id),
        ) else {
            return fallback;
        };
        let guild_id = GuildId::new(guild_id);
        let user_id = UserId::new(user_id);
        context
            .cache
            .guild(guild_id)
            .and_then(|guild| {
                guild
                    .members
                    .get(&user_id)
                    .map(|member| member.display_name().to_owned())
            })
            .or_else(|| {
                context
                    .cache
                    .user(user_id)
                    .map(|user| user.display_name().to_owned())
            })
            .unwrap_or(fallback)
    }

    async fn create_prediction_market_surface(
        &self,
        channel_id: i64,
        announcement: DiscordMessage,
        thread_name: &str,
        market_message: DiscordMessage,
    ) -> Result<PredictionMarketSurface, String> {
        let context = self.context()?;
        let channel_id = ChannelId::new(
            u64::try_from(channel_id)
                .map_err(|_| "Discord prediction channel ID is outside the supported range")?,
        );
        let announcement = apply_discord_mentions(announcement.response.clone(), &announcement);
        let channel_message = channel_id
            .send_message(
                (&context.cache, context.http.as_ref()),
                channel_response(announcement),
            )
            .await
            .map_err(|error| error.to_string())?;
        let thread = channel_id
            .create_thread_from_message(
                (&context.cache, context.http.as_ref()),
                channel_message.id,
                CreateThread::new(thread_name).auto_archive_duration(AutoArchiveDuration::OneWeek),
            )
            .await
            .map_err(|error| error.to_string())?;
        let market_response =
            apply_discord_mentions(market_message.response.clone(), &market_message);
        let market_message = thread
            .id
            .send_message(
                (&context.cache, context.http.as_ref()),
                channel_response(market_response),
            )
            .await
            .map_err(|error| error.to_string())?;
        match thread.id.pin(&context.http, market_message.id).await {
            Ok(()) => {}
            Err(serenity::Error::Http(error))
                if error
                    .status_code()
                    .is_some_and(|status| status.as_u16() == 403) => {}
            Err(error) => return Err(error.to_string()),
        }
        Ok(PredictionMarketSurface {
            channel_message_id: i64::try_from(channel_message.id.get())
                .map_err(|_| "Discord prediction announcement ID exceeds SQLite INTEGER")?,
            thread_id: i64::try_from(thread.id.get())
                .map_err(|_| "Discord prediction thread ID exceeds SQLite INTEGER")?,
            embed_message_id: i64::try_from(market_message.id.get())
                .map_err(|_| "Discord prediction message ID exceeds SQLite INTEGER")?,
        })
    }

    async fn edit_prediction_command_message(
        &self,
        thread_id: i64,
        message_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        let context = self.context()?;
        let channel_id = ChannelId::new(
            u64::try_from(thread_id)
                .map_err(|_| "Discord prediction thread ID is outside the supported range")?,
        );
        let message_id = MessageId::new(
            u64::try_from(message_id)
                .map_err(|_| "Discord prediction message ID is outside the supported range")?,
        );
        let mut fetched = match channel_id
            .message((&context.cache, context.http.as_ref()), message_id)
            .await
        {
            Ok(message) => message,
            Err(serenity::Error::Http(error))
                if error
                    .status_code()
                    .is_some_and(|status| status.as_u16() == 404) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        };
        ensure_prediction_thread_writable(&context, channel_id).await?;
        let response = apply_discord_mentions(message.response.clone(), &message);
        let attachments = response
            .attachments
            .iter()
            .map(serenity_attachment)
            .fold(EditAttachments::new(), EditAttachments::add);
        let mut edit = EditMessage::new()
            .embeds(response.embeds.iter().map(serenity_embed).collect())
            .attachments(attachments)
            .components(serenity_components(&response.components));
        if message.content_mode == crate::discord_transport::DiscordMessageContentMode::Replace {
            edit = edit.content(response.content);
        }
        if let Some(mentions) = serenity_allowed_mentions(&response.allowed_mentions) {
            edit = edit.allowed_mentions(mentions);
        }
        fetched
            .edit((&context.cache, context.http.as_ref()), edit)
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }

    async fn reopen_prediction_thread(&self, thread_id: i64) -> Result<(), String> {
        let context = self.context()?;
        let channel_id = ChannelId::new(
            u64::try_from(thread_id)
                .map_err(|_| "Discord prediction thread ID is outside the supported range")?,
        );
        channel_id
            .edit_thread(
                (&context.cache, context.http.as_ref()),
                EditThread::new()
                    .locked(false)
                    .archived(false)
                    .auto_archive_duration(AutoArchiveDuration::OneWeek),
            )
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }

    async fn archive_prediction_thread(&self, thread_id: i64) -> Result<(), String> {
        let context = self.context()?;
        let channel_id = ChannelId::new(
            u64::try_from(thread_id)
                .map_err(|_| "Discord prediction thread ID is outside the supported range")?,
        );
        match channel_id
            .edit_thread(
                (&context.cache, context.http.as_ref()),
                EditThread::new().locked(true).archived(true),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(serenity::Error::Http(error))
                if error
                    .status_code()
                    .is_some_and(|status| status.as_u16() == 403) =>
            {
                channel_id
                    .edit_thread(
                        (&context.cache, context.http.as_ref()),
                        EditThread::new().archived(true),
                    )
                    .await
                    .map(drop)
                    .map_err(|error| error.to_string())
            }
            Err(error) => Err(error.to_string()),
        }
    }
}

#[async_trait]
impl TriviaDiscordPort for SerenityDiscordTransport {
    async fn trivia_channel_is_gamba(
        &self,
        guild_id: i64,
        channel_id: i64,
    ) -> Result<bool, String> {
        PredictionCommandDiscordPort::prediction_channel_is_gamba(self, guild_id, channel_id).await
    }

    async fn trivia_user_avatar_url(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<Option<String>, String> {
        let context = self.context()?;
        let user_id = UserId::new(
            u64::try_from(user_id)
                .map_err(|_| "Discord trivia user ID is outside the supported range")?,
        );
        if guild_id != 0 {
            let guild_id = GuildId::new(
                u64::try_from(guild_id)
                    .map_err(|_| "Discord trivia guild ID is outside the supported range")?,
            );
            if let Ok(member) = guild_id
                .member((&context.cache, context.http.as_ref()), user_id)
                .await
            {
                return Ok(Some(member.face()));
            }
        }
        let user = user_id
            .to_user(&context.http)
            .await
            .map_err(|error| error.to_string())?;
        Ok(Some(user.face()))
    }

    async fn edit_trivia_message(
        &self,
        channel_id: u64,
        message_id: u64,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        let context = self.context().map_err(InteractionResponseError::new)?;
        ChannelId::new(channel_id)
            .edit_message(
                (&context.cache, context.http.as_ref()),
                MessageId::new(message_id),
                edit_channel_response(DiscordMessage::default_mentions(response)),
            )
            .await
            .map(drop)
            .map_err(response_error)
    }

    async fn delete_trivia_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        DiscordTransport::delete_message(self, channel_id, message_id).await
    }
}

#[async_trait]
impl PlayerTriviaDiscordPort for SerenityDiscordTransport {
    async fn player_trivia_channel_is_gamba(
        &self,
        guild_id: i64,
        channel_id: i64,
    ) -> Result<bool, String> {
        PredictionCommandDiscordPort::prediction_channel_is_gamba(self, guild_id, channel_id).await
    }

    fn player_trivia_non_bot_member_ids(
        &self,
        guild_id: i64,
    ) -> Result<Option<BTreeSet<i64>>, String> {
        let context = self.context()?;
        let guild_id = GuildId::new(
            u64::try_from(guild_id)
                .map_err(|_| "Discord player-trivia guild ID is outside the supported range")?,
        );
        let Some(guild) = context.cache.guild(guild_id) else {
            return Ok(None);
        };
        if guild.members.is_empty() {
            return Ok(None);
        }
        guild
            .members
            .values()
            .filter(|member| !member.user.bot)
            .map(|member| {
                i64::try_from(member.user.id.get()).map_err(|_| {
                    "Discord player-trivia member ID is outside the supported range".to_owned()
                })
            })
            .collect::<Result<BTreeSet<_>, _>>()
            .map(Some)
    }

    async fn player_trivia_user_avatar_url(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<Option<String>, String> {
        TriviaDiscordPort::trivia_user_avatar_url(self, user_id, guild_id).await
    }

    async fn edit_player_trivia_message(
        &self,
        receipt: InteractionMessageReceipt,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        let context = self.context().map_err(InteractionResponseError::new)?;
        ChannelId::new(receipt.channel_id)
            .edit_message(
                (&context.cache, context.http.as_ref()),
                MessageId::new(receipt.message_id),
                edit_channel_response(DiscordMessage::default_mentions(response)),
            )
            .await
            .map(drop)
            .map_err(response_error)
    }
}

#[async_trait]
impl ManaDiscordPort for SerenityDiscordTransport {
    async fn mana_channel_is_gamba(&self, guild_id: i64, channel_id: i64) -> Result<bool, String> {
        PredictionCommandDiscordPort::prediction_channel_is_gamba(self, guild_id, channel_id).await
    }

    fn mana_guild_members(&self, guild_id: i64) -> Result<Vec<ManaGuildMember>, String> {
        let context = self.context()?;
        let guild_id = GuildId::new(
            u64::try_from(guild_id)
                .map_err(|_| "Discord mana guild ID is outside the supported range")?,
        );
        let Some(guild) = context.cache.guild(guild_id) else {
            return Ok(Vec::new());
        };
        guild
            .members
            .values()
            .map(|member| serenity_mana_member(&guild, member))
            .collect()
    }

    async fn mana_guild_member(
        &self,
        guild_id: i64,
        user_id: i64,
    ) -> Result<Option<ManaGuildMember>, String> {
        let context = self.context()?;
        let guild_id = GuildId::new(
            u64::try_from(guild_id)
                .map_err(|_| "Discord mana guild ID is outside the supported range")?,
        );
        let user_id = UserId::new(
            u64::try_from(user_id)
                .map_err(|_| "Discord mana user ID is outside the supported range")?,
        );

        if let Some(member) = context.cache.guild(guild_id).and_then(|guild| {
            guild
                .members
                .get(&user_id)
                .map(|member| serenity_mana_member(&guild, member))
        }) {
            return member.map(Some);
        }

        let member = match guild_id
            .member((&context.cache, context.http.as_ref()), user_id)
            .await
        {
            Ok(member) => member,
            Err(serenity::Error::Http(error))
                if error
                    .status_code()
                    .is_some_and(|status| status.as_u16() == 404) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.to_string()),
        };
        let cached = context.cache.guild(guild_id).map(|guild| {
            let all_roles_resolved = member
                .roles
                .iter()
                .all(|role_id| guild.roles.contains_key(role_id));
            (serenity_mana_member(&guild, &member), all_roles_resolved)
        });
        if let Some((snapshot, true)) = cached {
            return snapshot.map(Some);
        }

        let roles = guild_id
            .roles(context.http.as_ref())
            .await
            .map_err(|error| error.to_string())?;
        Ok(Some(ManaGuildMember {
            user_id: i64::try_from(member.user.id.get())
                .map_err(|_| "Discord mana user ID exceeds SQLite INTEGER")?,
            display_name: member.display_name().to_owned(),
            avatar_url: Some(member.face()),
            role_names: member
                .roles
                .iter()
                .filter_map(|role_id| roles.get(role_id).map(|role| role.name.clone()))
                .collect(),
        }))
    }
}

fn serenity_mana_member(guild: &Guild, member: &Member) -> Result<ManaGuildMember, String> {
    Ok(ManaGuildMember {
        user_id: i64::try_from(member.user.id.get())
            .map_err(|_| "Discord mana user ID exceeds SQLite INTEGER")?,
        display_name: member.display_name().to_owned(),
        avatar_url: Some(member.face()),
        role_names: member
            .roles
            .iter()
            .filter_map(|role_id| guild.roles.get(role_id).map(|role| role.name.clone()))
            .collect(),
    })
}

async fn prediction_channel(
    context: &SerenityContext,
    guild_id: GuildId,
    channel_id: ChannelId,
) -> Result<Option<GuildChannel>, String> {
    if let Some(channel) = context.cache.guild(guild_id).and_then(|guild| {
        guild.channels.get(&channel_id).cloned().or_else(|| {
            guild
                .threads
                .iter()
                .find(|thread| thread.id == channel_id)
                .cloned()
        })
    }) {
        return Ok(Some(channel));
    }
    match context.http.get_channel(channel_id).await {
        Ok(Channel::Guild(channel)) => Ok(Some(channel)),
        Ok(Channel::Private(_)) => Ok(None),
        Err(serenity::Error::Http(error))
            if error
                .status_code()
                .is_some_and(|status| status.as_u16() == 404) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.to_string()),
        #[allow(unreachable_patterns)]
        Ok(_) => Ok(None),
    }
}

async fn ensure_prediction_thread_writable(
    context: &SerenityContext,
    channel_id: ChannelId,
) -> Result<(), String> {
    let channel = context.cache.guilds().into_iter().find_map(|guild_id| {
        context.cache.guild(guild_id).and_then(|guild| {
            guild
                .threads
                .iter()
                .find(|thread| thread.id == channel_id)
                .cloned()
        })
    });
    let channel = match channel {
        Some(channel) => Some(channel),
        None => match context.http.get_channel(channel_id).await {
            Ok(Channel::Guild(channel)) => Some(channel),
            Ok(Channel::Private(_)) => None,
            Err(error) => return Err(error.to_string()),
            #[allow(unreachable_patterns)]
            Ok(_) => None,
        },
    };
    if channel
        .as_ref()
        .and_then(|channel| channel.thread_metadata)
        .is_some_and(|metadata| metadata.archived)
    {
        channel_id
            .edit_thread(
                (&context.cache, context.http.as_ref()),
                EditThread::new()
                    .archived(false)
                    .auto_archive_duration(AutoArchiveDuration::OneWeek),
            )
            .await
            .map(drop)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[async_trait]
impl DiscordTransport for SerenityDiscordTransport {
    async fn fetch_message(
        &self,
        channel_id: u64,
        message_id: u64,
    ) -> Result<Option<DiscordMessageSnapshot>, String> {
        let context = self.context()?;
        let channel_id = ChannelId::new(channel_id);
        let message_id = MessageId::new(message_id);
        match channel_id
            .message((&context.cache, context.http.as_ref()), message_id)
            .await
        {
            Ok(message) => Ok(Some(DiscordMessageSnapshot {
                receipt: DiscordMessageReceipt {
                    channel_id: channel_id.get(),
                    message_id: message_id.get(),
                    jump_url: message.link(),
                },
                reactions: message
                    .reactions
                    .iter()
                    .map(|reaction| match &reaction.reaction_type {
                        ReactionType::Custom { id, name, .. } => DiscordEmoji {
                            name: name.clone().unwrap_or_default(),
                            id: Some(id.get()),
                        },
                        ReactionType::Unicode(name) => DiscordEmoji::unicode(name),
                        other => DiscordEmoji::unicode(other.to_string()),
                    })
                    .collect(),
            })),
            Err(serenity::Error::Http(error))
                if error
                    .status_code()
                    .is_some_and(|status| status.as_u16() == 404) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    async fn send_message(
        &self,
        channel_id: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        let context = self.context()?;
        let channel_id = ChannelId::new(channel_id);
        let response = apply_discord_mentions(message.response.clone(), &message);
        let sent = channel_id
            .send_message(
                (&context.cache, context.http.as_ref()),
                channel_response(response),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(DiscordMessageReceipt {
            channel_id: channel_id.get(),
            message_id: sent.id.get(),
            jump_url: sent.link(),
        })
    }

    async fn send_message_with_delivery_key(
        &self,
        channel_id: u64,
        delivery_key: &str,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        let context = self.context()?;
        let channel_id = ChannelId::new(channel_id);
        let response = apply_discord_mentions(message.response.clone(), &message);
        let sent = channel_id
            .send_message(
                (&context.cache, context.http.as_ref()),
                channel_response(response)
                    .nonce(Nonce::String(delivery_key.to_owned()))
                    .enforce_nonce(true),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(DiscordMessageReceipt {
            channel_id: channel_id.get(),
            message_id: sent.id.get(),
            jump_url: sent.link(),
        })
    }

    async fn find_message_by_delivery_key(
        &self,
        channel_id: u64,
        delivery_key: &str,
        after_unix_seconds: i64,
        limit: usize,
    ) -> Result<Option<DiscordMessageReceipt>, String> {
        if limit == 0 {
            return Ok(None);
        }
        let context = self.context()?;
        let channel_id = ChannelId::new(channel_id);
        let bot_user_id = context
            .http
            .get_current_user()
            .await
            .map_err(|error| error.to_string())?
            .id
            .get();
        let bounded_limit = limit.min(500);
        let mut before = None;
        let mut inspected = 0usize;
        let mut reached_history_boundary = false;
        while inspected < bounded_limit {
            let page_limit = (bounded_limit - inspected).min(100) as u8;
            let mut request = GetMessages::new().limit(page_limit);
            if let Some(message_id) = before {
                request = request.before(message_id);
            }
            let page = channel_id
                .messages(&context.http, request)
                .await
                .map_err(|error| error.to_string())?;
            if page.is_empty() {
                reached_history_boundary = true;
                break;
            }
            inspected = inspected.saturating_add(page.len());
            let page_complete = page.len() == page_limit as usize;
            let oldest_before_cutoff = page
                .last()
                .is_some_and(|message| message.timestamp.unix_timestamp() < after_unix_seconds);
            for message in &page {
                if message.timestamp.unix_timestamp() < after_unix_seconds
                    || message.author.id.get() != bot_user_id
                {
                    continue;
                }
                let nonce_matches = message.nonce.as_ref().is_some_and(|nonce| match nonce {
                    Nonce::String(value) => value == delivery_key,
                    Nonce::Number(value) => value.to_string() == delivery_key,
                });
                if nonce_matches {
                    return Ok(Some(DiscordMessageReceipt {
                        channel_id: message.channel_id.get(),
                        message_id: message.id.get(),
                        jump_url: message.link(),
                    }));
                }
            }
            if oldest_before_cutoff {
                reached_history_boundary = true;
                break;
            }
            if !page_complete {
                reached_history_boundary = true;
                break;
            }
            before = page.last().map(|message| message.id);
        }
        if !reached_history_boundary && inspected >= bounded_limit {
            return Err(format!(
                "Discord delivery history search reached its {bounded_limit}-message bound before the recovery time boundary"
            ));
        }
        Ok(None)
    }

    async fn send_thread_reply(
        &self,
        thread_id: u64,
        parent_message_id: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        let context = self.context()?;
        let channel_id = ChannelId::new(thread_id);
        let response = apply_discord_mentions(message.response.clone(), &message);
        let sent = channel_id
            .send_message(
                (&context.cache, context.http.as_ref()),
                channel_response(response)
                    .reference_message((channel_id, MessageId::new(parent_message_id))),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(DiscordMessageReceipt {
            channel_id: channel_id.get(),
            message_id: sent.id.get(),
            jump_url: sent.link(),
        })
    }

    async fn edit_message(
        &self,
        channel_id: u64,
        message_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        let context = self.context()?;
        ChannelId::new(channel_id)
            .edit_message(
                (&context.cache, context.http.as_ref()),
                MessageId::new(message_id),
                edit_channel_response(message),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn delete_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        let context = self.context()?;
        ChannelId::new(channel_id)
            .delete_message(&context.http, MessageId::new(message_id))
            .await
            .map_err(|error| error.to_string())
    }

    async fn create_public_thread(
        &self,
        channel_id: u64,
        message_id: u64,
        name: &str,
    ) -> Result<u64, String> {
        let context = self.context()?;
        ChannelId::new(channel_id)
            .create_thread_from_message(
                (&context.cache, context.http.as_ref()),
                MessageId::new(message_id),
                CreateThread::new(name),
            )
            .await
            .map(|thread| thread.id.get())
            .map_err(|error| error.to_string())
    }

    async fn create_private_thread(
        &self,
        channel_id: u64,
        name: &str,
        member_ids: &[u64],
    ) -> Result<u64, String> {
        let context = self.context()?;
        let thread = ChannelId::new(channel_id)
            .create_thread(
                (&context.cache, context.http.as_ref()),
                CreateThread::new(name)
                    .kind(ChannelType::PrivateThread)
                    .invitable(false)
                    .auto_archive_duration(AutoArchiveDuration::OneDay),
            )
            .await
            .map_err(|error| error.to_string())?;

        // Discord's REST endpoint is per member.  Keep at most four requests
        // in flight, matching the Python cog's THREAD_MEMBER_CONCURRENCY;
        // a single inaccessible member must not make the private thread
        // public or discard the other successful additions.
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
        let mut tasks = Vec::new();
        let mut unique_members = BTreeSet::new();
        for member_id in member_ids.iter().copied() {
            if !unique_members.insert(member_id) {
                continue;
            }
            let permit = std::sync::Arc::clone(&semaphore);
            let http = Arc::clone(&context.http);
            let thread_id = thread.id;
            tasks.push(tokio::spawn(async move {
                let _permit = permit
                    .acquire_owned()
                    .await
                    .map_err(|error| error.to_string())?;
                thread_id
                    .add_thread_member(http.as_ref(), UserId::new(member_id))
                    .await
                    .map_err(|error| error.to_string())
            }));
        }
        for task in tasks {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => warn!(%error, "could not add Mafia member to private thread"),
                Err(error) => warn!(%error, "Mafia private-thread member task panicked"),
            }
        }
        Ok(thread.id.get())
    }

    async fn add_private_thread_members(
        &self,
        thread_id: u64,
        member_ids: &[u64],
    ) -> Result<(), String> {
        let context = self.context()?;
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
        let mut tasks = Vec::new();
        let mut unique_members = BTreeSet::new();
        for member_id in member_ids.iter().copied() {
            if !unique_members.insert(member_id) {
                continue;
            }
            let permit = std::sync::Arc::clone(&semaphore);
            let http = Arc::clone(&context.http);
            let thread_id = ChannelId::new(thread_id);
            tasks.push(tokio::spawn(async move {
                let _permit = permit
                    .acquire_owned()
                    .await
                    .map_err(|error| error.to_string())?;
                thread_id
                    .add_thread_member(http.as_ref(), UserId::new(member_id))
                    .await
                    .map_err(|error| error.to_string())
            }));
        }
        for task in tasks {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => warn!(%error, "could not add Mafia member to private thread"),
                Err(error) => warn!(%error, "Mafia private-thread member task panicked"),
            }
        }
        Ok(())
    }

    async fn add_thread_member(&self, thread_id: u64, member_id: u64) -> Result<(), String> {
        let context = self.context()?;
        ChannelId::new(thread_id)
            .add_thread_member(context.http.as_ref(), UserId::new(member_id))
            .await
            .map_err(|error| error.to_string())
    }

    async fn mafia_channel_available(
        &self,
        guild_id: u64,
        channel_id: u64,
    ) -> Result<bool, String> {
        let context = self.context()?;
        let guild_id = GuildId::new(guild_id);
        let channel_id = ChannelId::new(channel_id);
        let Some(guild) = context.cache.guild(guild_id) else {
            return Ok(false);
        };
        let Some(channel) = guild.channels.get(&channel_id) else {
            return Ok(false);
        };
        if !matches!(channel.kind, ChannelType::Text | ChannelType::News) {
            return Ok(false);
        }
        let Some(member) = guild.members.get(&context.cache.current_user().id) else {
            // A READY cache without the bot's guild member cannot prove an
            // overwrite, but the channel itself is still a valid candidate;
            // Discord remains the final permission authority on send.
            return Ok(true);
        };
        Ok(guild
            .user_permissions_in(channel, member)
            .contains(Permissions::SEND_MESSAGES))
    }

    async fn guild_channel_exists(&self, guild_id: u64, channel_id: u64) -> Result<bool, String> {
        let context = self.context()?;
        let channel_id = ChannelId::new(channel_id);
        let Some(guild) = context.cache.guild(GuildId::new(guild_id)) else {
            return Ok(false);
        };
        Ok(guild.channels.contains_key(&channel_id)
            || guild.threads.iter().any(|thread| thread.id == channel_id))
    }

    async fn mafia_gamba_channel_id(&self, guild_id: u64) -> Result<Option<u64>, String> {
        let context = self.context()?;
        let guild_id = GuildId::new(guild_id);
        let Some(guild) = context.cache.guild(guild_id) else {
            return Ok(None);
        };
        let bot_member = guild.members.get(&context.cache.current_user().id);
        let mut channels = guild.channels.values().cloned().collect::<Vec<_>>();
        channels.sort_unstable_by_key(|channel| (channel.position, channel.id.get()));
        let configured_id = self
            .gamba_channel_id
            .and_then(|channel_id| u64::try_from(channel_id).ok());
        Ok(channels
            .into_iter()
            .filter(|channel| {
                matches!(channel.kind, ChannelType::Text | ChannelType::News)
                    && (configured_id == Some(channel.id.get())
                        || channel.name.to_ascii_lowercase().contains("gamba"))
            })
            .find(|channel| {
                bot_member.is_none_or(|member| {
                    guild
                        .user_permissions_in(channel, member)
                        .contains(Permissions::SEND_MESSAGES)
                })
            })
            .map(|channel| channel.id.get()))
    }

    async fn channel_is_gamba(&self, guild_id: u64, channel_id: u64) -> Result<bool, String> {
        PredictionCommandDiscordPort::prediction_channel_is_gamba(
            self,
            i64::try_from(guild_id).map_err(|_| "Discord gamba guild ID exceeds SQLite INTEGER")?,
            i64::try_from(channel_id)
                .map_err(|_| "Discord gamba channel ID exceeds SQLite INTEGER")?,
        )
        .await
    }

    async fn pin_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        let context = self.context()?;
        ChannelId::new(channel_id)
            .pin(&context.http, MessageId::new(message_id))
            .await
            .map_err(|error| error.to_string())
    }

    async fn archive_thread(&self, thread_id: u64, name: &str, locked: bool) -> Result<(), String> {
        let context = self.context()?;
        ChannelId::new(thread_id)
            .edit_thread(
                (&context.cache, context.http.as_ref()),
                EditThread::new().name(name).archived(true).locked(locked),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn edit_thread(
        &self,
        thread_id: u64,
        name: &str,
        archived: bool,
        locked: bool,
    ) -> Result<(), String> {
        let context = self.context()?;
        ChannelId::new(thread_id)
            .edit_thread(
                (&context.cache, context.http.as_ref()),
                thread_lifecycle_edit(name, archived, locked),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn add_reaction(
        &self,
        channel_id: u64,
        message_id: u64,
        emoji: &DiscordEmoji,
    ) -> Result<(), String> {
        let context = self.context()?;
        ChannelId::new(channel_id)
            .create_reaction(
                &context.http,
                MessageId::new(message_id),
                discord_reaction_type(emoji),
            )
            .await
            .map_err(|error| error.to_string())
    }

    async fn remove_reaction(
        &self,
        channel_id: u64,
        message_id: u64,
        emoji: &DiscordEmoji,
        user_id: u64,
    ) -> Result<(), String> {
        let context = self.context()?;
        ChannelId::new(channel_id)
            .delete_reaction(
                &context.http,
                MessageId::new(message_id),
                Some(UserId::new(user_id)),
                discord_reaction_type(emoji),
            )
            .await
            .map_err(|error| error.to_string())
    }

    async fn clear_reaction(
        &self,
        channel_id: u64,
        message_id: u64,
        emoji: &DiscordEmoji,
    ) -> Result<(), String> {
        let context = self.context()?;
        ChannelId::new(channel_id)
            .delete_reaction_emoji(
                &context.http,
                MessageId::new(message_id),
                discord_reaction_type(emoji),
            )
            .await
            .map_err(|error| error.to_string())
    }

    async fn unpin_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        let context = self.context()?;
        ChannelId::new(channel_id)
            .unpin(&context.http, MessageId::new(message_id))
            .await
            .map_err(|error| error.to_string())
    }

    async fn pinned_messages(
        &self,
        channel_id: u64,
    ) -> Result<Vec<crate::discord_transport::DiscordPinnedMessage>, String> {
        let context = self.context()?;
        ChannelId::new(channel_id)
            .pins(&context.http)
            .await
            .map(|messages| {
                messages
                    .into_iter()
                    .map(|message| crate::discord_transport::DiscordPinnedMessage {
                        message_id: message.id.get(),
                        author_id: message.author.id.get(),
                    })
                    .collect()
            })
            .map_err(|error| error.to_string())
    }

    async fn send_direct_message(
        &self,
        user_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        let context = self.context()?;
        let response = apply_discord_mentions(message.response.clone(), &message);
        UserId::new(user_id)
            .direct_message(
                (&context.cache, context.http.as_ref()),
                channel_response(response),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn send_direct_message_with_receipt(
        &self,
        user_id: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, DiscordDirectMessageError> {
        let context = self
            .context()
            .map_err(DiscordDirectMessageError::from_message)?;
        let response = apply_discord_mentions(message.response.clone(), &message);
        UserId::new(user_id)
            .direct_message(
                (&context.cache, context.http.as_ref()),
                channel_response(response),
            )
            .await
            .map(|sent| DiscordMessageReceipt {
                channel_id: sent.channel_id.get(),
                message_id: sent.id.get(),
                jump_url: sent.link(),
            })
            .map_err(|error| {
                let kind = match &error {
                    serenity::Error::Http(http)
                        if http
                            .status_code()
                            .is_some_and(|status| status.as_u16() == 403) =>
                    {
                        DiscordDirectMessageErrorKind::Forbidden
                    }
                    _ => DiscordDirectMessageErrorKind::Unavailable,
                };
                DiscordDirectMessageError {
                    kind,
                    message: error.to_string(),
                }
            })
    }

    async fn edit_direct_message(
        &self,
        receipt: &DiscordMessageReceipt,
        message: DiscordMessage,
    ) -> Result<(), String> {
        let context = self.context()?;
        ChannelId::new(receipt.channel_id)
            .edit_message(
                (&context.cache, context.http.as_ref()),
                MessageId::new(receipt.message_id),
                edit_channel_response(message),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn reaction_users(
        &self,
        channel_id: u64,
        message_id: u64,
        emoji: &DiscordEmoji,
    ) -> Result<Vec<DiscordUserSnapshot>, String> {
        let context = self.context()?;
        ChannelId::new(channel_id)
            .reaction_users(
                &context.http,
                MessageId::new(message_id),
                discord_reaction_type(emoji),
                Some(100),
                None,
            )
            .await
            .map(|users| {
                users
                    .into_iter()
                    .map(|user| DiscordUserSnapshot {
                        user_id: user.id.get(),
                        display_name: user.display_name().to_owned(),
                        is_bot: user.bot,
                    })
                    .collect()
            })
            .map_err(|error| error.to_string())
    }

    async fn bot_user_id(&self) -> Result<Option<u64>, String> {
        let context = self.context()?;
        Ok(Some(context.cache.current_user().id.get()))
    }

    async fn guild_name(&self, guild_id: u64) -> Result<Option<String>, String> {
        let context = self.context()?;
        let guild_id = GuildId::new(guild_id);
        if let Some(name) = context
            .cache
            .guild(guild_id)
            .map(|guild| guild.name.clone())
        {
            return Ok(Some(name));
        }
        // A reconnect or a guild not yet delivered by GUILD_CREATE leaves the
        // cache empty. Falling back to HTTP, like `user` and `guild_member` do,
        // keeps a notice from silently degrading to unqualified copy.
        fetch_guild_name(&context.http, guild_id).await
    }

    async fn user(&self, user_id: u64) -> Result<Option<DiscordUserSnapshot>, String> {
        let context = self.context()?;
        let user_id = UserId::new(user_id);
        if let Some(user) = context.cache.user(user_id) {
            return Ok(Some(DiscordUserSnapshot {
                user_id: user.id.get(),
                display_name: user.display_name().to_owned(),
                is_bot: user.bot,
            }));
        }
        match user_id.to_user(&context.http).await {
            Ok(user) => Ok(Some(DiscordUserSnapshot {
                user_id: user.id.get(),
                display_name: user.display_name().to_owned(),
                is_bot: user.bot,
            })),
            Err(serenity::Error::Http(error))
                if error
                    .status_code()
                    .is_some_and(|status| status.as_u16() == 404) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    async fn guild_member(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
        let context = self.context()?;
        let guild_id = GuildId::new(guild_id);
        let user_id = UserId::new(user_id);
        let member = match guild_id
            .member((&context.cache, context.http.as_ref()), user_id)
            .await
        {
            Ok(member) => member,
            Err(serenity::Error::Http(error))
                if error
                    .status_code()
                    .is_some_and(|status| status.as_u16() == 404) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.to_string()),
        };
        let (presence, in_voice, deafened, activities) = context.cache.guild(guild_id).map_or(
            (DiscordPresence::Unknown, false, member.deaf, Vec::new()),
            |guild| {
                let voice = guild.voice_states.get(&user_id);
                let presence = guild.presences.get(&user_id);
                (
                    presence.map_or(DiscordPresence::Unknown, |presence| match presence.status {
                        OnlineStatus::Online => DiscordPresence::Online,
                        OnlineStatus::Idle => DiscordPresence::Idle,
                        OnlineStatus::DoNotDisturb => DiscordPresence::DoNotDisturb,
                        OnlineStatus::Offline | OnlineStatus::Invisible => DiscordPresence::Offline,
                        _ => DiscordPresence::Unknown,
                    }),
                    voice.is_some_and(|voice| voice.channel_id.is_some()),
                    voice.is_some_and(|voice| voice.deaf || voice.self_deaf),
                    presence.map_or_else(Vec::new, |presence| {
                        presence
                            .activities
                            .iter()
                            .map(|activity| activity.name.clone())
                            .collect()
                    }),
                )
            },
        );
        Ok(Some(DiscordGuildMemberSnapshot {
            user_id: user_id.get(),
            display_name: member.display_name().to_owned(),
            presence,
            in_voice,
            deafened,
            activities,
        }))
    }

    fn cached_guild_member_display_name(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<Option<String>, String> {
        if guild_id == 0 || user_id == 0 {
            return Ok(None);
        }
        let context = self.context()?;
        let guild_id = GuildId::new(guild_id);
        let user_id = UserId::new(user_id);
        Ok(context.cache.guild(guild_id).and_then(|guild| {
            guild
                .members
                .get(&user_id)
                .map(|member| member.display_name().to_owned())
        }))
    }

    async fn channel_parent_id(
        &self,
        guild_id: u64,
        channel_id: u64,
    ) -> Result<Option<u64>, String> {
        let context = self.context()?;
        let channel_id = ChannelId::new(channel_id);
        if let Some(parent_id) = context
            .cache
            .guild(GuildId::new(guild_id))
            .and_then(|guild| {
                guild
                    .threads
                    .iter()
                    .find(|channel| channel.id == channel_id)
                    .and_then(|channel| channel.parent_id)
            })
        {
            return Ok(Some(parent_id.get()));
        }
        match context.http.get_channel(channel_id).await {
            Ok(Channel::Guild(channel)) => Ok(channel.parent_id.map(|parent| parent.get())),
            Ok(Channel::Private(_)) => Ok(None),
            Err(serenity::Error::Http(error))
                if error
                    .status_code()
                    .is_some_and(|status| status.as_u16() == 404) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.to_string()),
            #[allow(unreachable_patterns)]
            Ok(_) => Ok(None),
        }
    }

    async fn streaming_member_ids(
        &self,
        guild_id: u64,
        user_ids: &[u64],
    ) -> Result<BTreeSet<u64>, String> {
        let context = self.context()?;
        let guild_id = GuildId::new(guild_id);
        let requested = user_ids.iter().copied().collect::<BTreeSet<_>>();
        Ok(context
            .cache
            .guild(guild_id)
            .map_or_else(BTreeSet::new, |guild| {
                guild
                    .presences
                    .iter()
                    .filter(|(user_id, presence)| {
                        requested.contains(&user_id.get())
                            && presence
                                .activities
                                .iter()
                                .any(|activity| is_streaming_activity(activity.kind))
                    })
                    .map(|(user_id, _)| user_id.get())
                    .collect()
            }))
    }
}

#[async_trait]
impl WrappedDiscordPort for SerenityDiscordTransport {
    async fn wrapped_profile(
        &self,
        guild_id: u64,
        user_id: u64,
        include_avatar: bool,
    ) -> Result<Option<WrappedDiscordProfile>, String> {
        let context = self.context()?;
        let guild_id = GuildId::new(guild_id);
        let user_id = UserId::new(user_id);
        let member = match guild_id
            .member((&context.cache, context.http.as_ref()), user_id)
            .await
        {
            Ok(member) => member,
            Err(serenity::Error::Http(error))
                if error
                    .status_code()
                    .is_some_and(|status| status.as_u16() == 404) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.to_string()),
        };
        let display_name = member.display_name().to_owned();
        let avatar_png = if include_avatar {
            if let Some(avatar) = member.avatar {
                let url = format!(
                    "https://cdn.discordapp.com/guilds/{}/users/{}/avatars/{avatar}.png?size=64",
                    guild_id.get(),
                    user_id.get(),
                );
                match download_wrapped_avatar(&url).await {
                    Ok(bytes) => Some(bytes),
                    Err(error) => {
                        warn!(%error, user_id = user_id.get(), "wrapped avatar download failed");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        Ok(Some(WrappedDiscordProfile {
            display_name,
            avatar_png,
        }))
    }
}

async fn download_wrapped_avatar(url: &str) -> Result<Vec<u8>, String> {
    let client = WRAPPED_AVATAR_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::limited(3))
            .user_agent("cama-rust-wrapped/1")
            .build()
            .expect("static Wrapped avatar HTTP client configuration")
    });
    let mut response = client
        .get(url)
        .timeout(WRAPPED_AVATAR_TIMEOUT)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Discord CDN returned HTTP {} for Wrapped avatar",
            response.status()
        ));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("image/png"))
    {
        return Err(format!(
            "Discord CDN returned unsupported Wrapped avatar content type {content_type:?}"
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > WRAPPED_AVATAR_MAX_BYTES as u64)
    {
        return Err("Wrapped avatar exceeded the 2 MiB limit".to_owned());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if bytes.len().saturating_add(chunk.len()) > WRAPPED_AVATAR_MAX_BYTES {
            return Err("Wrapped avatar exceeded the 2 MiB limit".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn serenity_attachment(attachment: &InteractionAttachment) -> CreateAttachment {
    let result = CreateAttachment::bytes(attachment.bytes.clone(), &attachment.filename);
    if let Some(description) = &attachment.description {
        result.description(description)
    } else {
        result
    }
}

fn thread_lifecycle_edit(name: &str, archived: bool, locked: bool) -> EditThread<'_> {
    EditThread::new()
        .name(name)
        .archived(archived)
        .locked(locked)
}

const fn is_streaming_activity(kind: ActivityType) -> bool {
    matches!(kind, ActivityType::Streaming)
}

fn response_error(error: serenity::Error) -> InteractionResponseError {
    let status_code = match &error {
        serenity::Error::Http(error) => error.status_code().map(|status| status.as_u16()),
        _ => None,
    };
    InteractionResponseError::with_status_code(error.to_string(), status_code)
}

fn dig_public_send_failure(error: serenity::Error) -> DigPublicSendFailure {
    let status = match &error {
        serenity::Error::Http(error) => error.status_code().map(|status| status.as_u16()),
        _ => None,
    };
    let kind = dig_public_send_failure_kind(status);
    DigPublicSendFailure {
        kind,
        message: error.to_string(),
    }
}

const fn dig_public_send_failure_kind(status: Option<u16>) -> DigPublicSendFailureKind {
    match status {
        // A concrete client-error response proves Discord rejected the
        // request. Request-timeout and rate-limit responses remain recovery
        // only because their transport/retry timing is not a safe fallback
        // boundary.
        Some(400..=499) if !matches!(status, Some(408 | 429)) => DigPublicSendFailureKind::Rejected,
        _ => DigPublicSendFailureKind::Ambiguous,
    }
}

#[cfg(all(test, feature = "runtime-test-match"))]
#[path = "serenity_transport/tests.rs"]
mod tests;
