//! Live `/duel` commands and restart-safe response controls.
//!
//! The command handler moves every SQLite transition to Tokio's blocking
//! pool, uses the same production flavor generator as the supervised duel
//! worker, and binds the real Discord message ID before enabling persistent
//! controls. Failed initial delivery atomically refunds both stake and fee.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::ai_services::AIService;
use cama_app::duel::{
    CommandError, DuelApplicationService, DuelEvolutionPort, FlavorEvent, TRIAL_BY_COMBAT_RULES,
};
use cama_db::duel_repository::{DuelChallenge, DuelChallengeRepository, DuelStatus, DuelTrial};
use cama_db::pet_evolution_repository::PetEvolutionRepository;
use cama_domain::pet_evolution::PetActivity;

use crate::application_config::ApplicationConfig;
use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::duel_challenges_worker::{DuelFlavorRuntimePort, ProductionDuelFlavorRuntime};
use crate::gateway_events::{
    GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};
use crate::option_ext::{integer_option, string_option, user_option};
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionActionRow, InteractionButton, InteractionButtonStyle, InteractionEmbed,
    InteractionHandler, InteractionHandlerError, InteractionMessageReceipt, InteractionOption,
    InteractionRequest, InteractionResponder, InteractionResponse, InteractionValue,
    RegistrationError, RegistrationProvider, RegistryBuilder,
};

const COMPONENT_PREFIX: &str = "duel:";
const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;

#[derive(Clone)]
pub struct DuelRegistrationProvider {
    handler: Arc<DuelInteractionHandler>,
}

impl DuelRegistrationProvider {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn DiscordTransport>,
        ai_service: Option<Arc<AIService>>,
    ) -> Self {
        let database_path = database_path.as_ref().to_path_buf();
        Self {
            handler: Arc::new(DuelInteractionHandler {
                repository: DuelChallengeRepository::new(&database_path),
                evolution: PetEvolutionRepository::new(&database_path),
                flavor: Arc::new(ProductionDuelFlavorRuntime::new(
                    &database_path,
                    ai_service,
                    config.values.ai_features_enabled,
                )),
                discord,
                admin_user_ids: config.identities.admin_user_ids.iter().copied().collect(),
            }),
        }
    }

    /// Restore every persistent duel response surface after READY/resume.
    #[must_use]
    pub fn gateway_observer(&self) -> Arc<dyn GatewayEventObserver> {
        Arc::new(DuelGatewayObserver {
            handler: Arc::clone(&self.handler),
        })
    }
}

impl RegistrationProvider for DuelRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "duel".to_owned(),
            description: "Challenges of honor".to_owned(),
            options: duel_subcommands(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

struct DuelInteractionHandler {
    repository: DuelChallengeRepository,
    evolution: PetEvolutionRepository,
    flavor: Arc<dyn DuelFlavorRuntimePort>,
    discord: Arc<dyn DiscordTransport>,
    admin_user_ids: BTreeSet<i64>,
}

struct DuelGatewayObserver {
    handler: Arc<DuelInteractionHandler>,
}

#[async_trait]
impl GatewayEventObserver for DuelGatewayObserver {
    fn name(&self) -> &'static str {
        "duel-pending-recovery"
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        self.handler.recover_pending(context.guild_ids()).await
    }
}

#[derive(Clone)]
struct RepositoryDuelEvolution(PetEvolutionRepository);

impl DuelEvolutionPort for RepositoryDuelEvolution {
    fn record_activity(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        activity: PetActivity,
        source_key: &str,
        occurred_at: i64,
    ) {
        // Python treats upbringing scoring as a fail-open post-settlement
        // side effect. The durable duel result is never rolled back here.
        let _ = self.0.record_activity(
            discord_id,
            Some(guild_id),
            activity,
            source_key,
            occurred_at,
        );
    }
}

#[derive(Clone, Debug)]
struct DuelCommandContext {
    user_id: i64,
    guild_id: Option<i64>,
    channel_id: Option<i64>,
    member_permissions: Option<u64>,
    options: Vec<InteractionOption>,
}

#[async_trait]
impl InteractionHandler for DuelInteractionHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command {
                name,
                user_id,
                guild_id,
                channel_id,
                member_permissions,
                options,
                ..
            } => {
                if name != "duel" {
                    return Err(format!("duel handler received command {name:?}").into());
                }
                self.handle_command(
                    DuelCommandContext {
                        user_id: signed_id(user_id, "user")?,
                        guild_id: guild_id.map(|id| signed_id(id, "guild")).transpose()?,
                        channel_id: channel_id.map(|id| signed_id(id, "channel")).transpose()?,
                        member_permissions,
                        options,
                    },
                    responder,
                )
                .await
            }
            InteractionRequest::Component {
                custom_id,
                user_id,
                guild_id,
                channel_id,
                member_permissions,
                ..
            } => {
                self.handle_component(
                    DuelCommandContext {
                        user_id: signed_id(user_id, "user")?,
                        guild_id: guild_id.map(|id| signed_id(id, "guild")).transpose()?,
                        channel_id: channel_id.map(|id| signed_id(id, "channel")).transpose()?,
                        member_permissions,
                        options: Vec::new(),
                    },
                    &custom_id,
                    responder,
                )
                .await
            }
            InteractionRequest::Modal { .. } => Err("duel extension has no modal routes".into()),
            InteractionRequest::Autocomplete { .. } => {
                Err("duel extension has no autocomplete routes".into())
            }
        }
        .map_err(Into::into)
    }
}

impl DuelInteractionHandler {
    async fn recover_pending(&self, live_guild_ids: &[u64]) -> ReadyRecoveryReport {
        let live_guild_ids = live_guild_ids
            .iter()
            .filter_map(|guild_id| i64::try_from(*guild_id).ok())
            .collect::<BTreeSet<_>>();
        let repository = self.repository.clone();
        let pending = match tokio::task::spawn_blocking(move || {
            repository
                .list_pending_all()
                .map_err(|error| error.to_string())
        })
        .await
        {
            Ok(Ok(pending)) => pending,
            Ok(Err(message)) => {
                return ReadyRecoveryReport {
                    observer: "duel-pending-recovery",
                    guilds_attempted: live_guild_ids.len(),
                    guilds_refreshed: 0,
                    guilds_superseded: 0,
                    members_refreshed: 0,
                    failures: vec![ReadyRecoveryFailure {
                        guild_id: 0,
                        message,
                    }],
                };
            }
            Err(error) => {
                return ReadyRecoveryReport {
                    observer: "duel-pending-recovery",
                    guilds_attempted: live_guild_ids.len(),
                    guilds_refreshed: 0,
                    guilds_superseded: 0,
                    members_refreshed: 0,
                    failures: vec![ReadyRecoveryFailure {
                        guild_id: 0,
                        message: format!("duel recovery task failed: {error}"),
                    }],
                };
            }
        };
        let pending = pending
            .into_iter()
            .filter(|challenge| live_guild_ids.contains(&challenge.guild_id))
            .collect::<Vec<_>>();
        let attempted_guilds = pending
            .iter()
            .map(|challenge| challenge.guild_id)
            .collect::<BTreeSet<_>>();
        let mut refreshed_guilds = BTreeSet::new();
        let mut failures = Vec::new();

        for challenge in pending {
            let result = if let Some(message_id) = challenge.message_id {
                self.restore_bound_controls(&challenge, message_id).await
            } else {
                self.restore_unbound_challenge(&challenge).await
            };
            match result {
                Ok(()) => {
                    refreshed_guilds.insert(challenge.guild_id);
                }
                Err(message) => failures.push(ReadyRecoveryFailure {
                    guild_id: u64::try_from(challenge.guild_id).unwrap_or_default(),
                    message: format!("challenge #{}: {message}", challenge.challenge_id),
                }),
            }
        }
        ReadyRecoveryReport {
            observer: "duel-pending-recovery",
            guilds_attempted: attempted_guilds.len(),
            guilds_refreshed: refreshed_guilds.len(),
            guilds_superseded: 0,
            members_refreshed: 0,
            failures,
        }
    }

    async fn restore_bound_controls(
        &self,
        challenge: &DuelChallenge,
        message_id: i64,
    ) -> Result<(), String> {
        self.discord
            .edit_message(
                unsigned_id(challenge.channel_id, "channel")?,
                unsigned_id(message_id, "message")?,
                DiscordMessage::silent(
                    InteractionResponse::message("")
                        .action_row(challenge_buttons(challenge.challenge_id)),
                )
                .preserving_body(),
            )
            .await
    }

    async fn restore_unbound_challenge(&self, challenge: &DuelChallenge) -> Result<(), String> {
        let flavor = self.flavor.generate(FlavorEvent::Issued, challenge).await;
        let channel_id = unsigned_id(challenge.channel_id, "channel")?;
        let recipient_id = unsigned_id(challenge.recipient_id, "recipient")?;
        let sent = self
            .discord
            .send_message(
                channel_id,
                DiscordMessage::mentioning(
                    InteractionResponse::message(format!("<@{}>", challenge.recipient_id))
                        .embed(challenge_embed(challenge, &flavor)),
                    BTreeSet::from([recipient_id]),
                ),
            )
            .await;
        let receipt = match sent {
            Ok(receipt) => receipt,
            Err(error) => {
                self.mark_recovery_delivery_failed(challenge).await?;
                return Err(format!(
                    "replacement delivery failed and was refunded: {error}"
                ));
            }
        };
        let interaction_receipt = InteractionMessageReceipt {
            message_id: receipt.message_id,
            channel_id: receipt.channel_id,
            delivery: crate::registration::InteractionMessageDelivery::ChannelFallback,
        };
        match self
            .bind_challenge(challenge.clone(), challenge.guild_id, interaction_receipt)
            .await
        {
            Ok(_) => {
                let _ = self
                    .restore_bound_controls(
                        challenge,
                        i64::try_from(receipt.message_id)
                            .map_err(|_| "Discord replacement message ID exceeds SQLite range")?,
                    )
                    .await;
                Ok(())
            }
            Err(CommandError::Invalid(_)) => {
                let _ = self
                    .discord
                    .delete_message(receipt.channel_id, receipt.message_id)
                    .await;
                Ok(())
            }
            Err(error) => {
                let _ = self
                    .discord
                    .delete_message(receipt.channel_id, receipt.message_id)
                    .await;
                self.mark_recovery_delivery_failed(challenge).await?;
                Err(format!("replacement bind failed and was refunded: {error}"))
            }
        }
    }

    async fn mark_recovery_delivery_failed(&self, challenge: &DuelChallenge) -> Result<(), String> {
        let repository = self.repository.clone();
        let evolution = self.evolution.clone();
        let challenge_id = challenge.challenge_id;
        let guild_id = challenge.guild_id;
        let actor_id = challenge.challenger_id;
        match tokio::task::spawn_blocking(move || {
            let now = unix_now();
            let mut service = DuelApplicationService::with_evolution(
                repository,
                move || now as f64,
                RepositoryDuelEvolution(evolution),
            );
            service.mark_delivery_failed(challenge_id, guild_id, actor_id)
        })
        .await
        .map_err(|error| format!("duel recovery-refund task failed: {error}"))?
        {
            Ok(_) | Err(CommandError::Invalid(_)) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    async fn handle_command(
        &self,
        context: DuelCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let (subcommand, options) = leaf_command(&context.options)?;
        let options = options.to_vec();
        match subcommand.as_str() {
            "issue" => self.issue(context, &options, responder).await,
            "respond" => self.respond(context, &options, responder).await,
            "list" => self.list(context, responder).await,
            "resolve" => self.resolve(context, &options, responder).await,
            _ => Err(format!("unknown /duel subcommand {subcommand:?}")),
        }
    }

    async fn handle_component(
        &self,
        context: DuelCommandContext,
        custom_id: &str,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let mut parts = custom_id.split(':');
        if parts.next() != Some("duel") {
            return Err("invalid duel component prefix".to_owned());
        }
        let challenge_id = parts
            .next()
            .ok_or_else(|| "duel component is missing a challenge ID".to_owned())?
            .parse::<i64>()
            .map_err(|_| "duel component challenge ID is invalid".to_owned())?;
        let choice = parts
            .next()
            .ok_or_else(|| "duel component is missing a response".to_owned())?;
        if parts.next().is_some() {
            return Err("duel component has unexpected fields".to_owned());
        }
        let Some(guild_id) = context.guild_id else {
            return respond_ephemeral(&responder, "This command must be used in a server.").await;
        };
        let repository = self.repository.clone();
        let pending = tokio::task::spawn_blocking(move || {
            repository
                .get_challenge(challenge_id, Some(guild_id))
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("duel component preflight task failed: {error}"))??;
        let valid = pending.is_some_and(|challenge| {
            challenge.challenge_id == challenge_id
                && challenge.guild_id == guild_id
                && challenge.status == DuelStatus::Pending
                && challenge.message_id.is_some()
                && challenge.recipient_id == context.user_id
        });
        if !valid {
            return respond_ephemeral(
                &responder,
                "This duel challenge is unavailable or is not yours to answer.",
            )
            .await;
        }
        // Act on the challenge this button was rendered for, not on whatever
        // is pending now.
        self.respond_choice(context, choice, Some(challenge_id), responder)
            .await
    }

    async fn issue(
        &self,
        context: DuelCommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let (Some(guild_id), Some(channel_id)) = (context.guild_id, context.channel_id) else {
            return respond_ephemeral(&responder, "This command must be used in a server.").await;
        };
        let recipient = user_option(options, "player")?
            .ok_or_else(|| "/duel issue requires player".to_owned())?;
        let (recipient_id, recipient_is_bot) = (recipient.id, recipient.is_bot.unwrap_or(false));
        let wager = integer_option(options, "wager")
            .ok_or_else(|| "/duel issue requires wager".to_owned())?;
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;

        let repository = self.repository.clone();
        let evolution = self.evolution.clone();
        let actor_id = context.user_id;
        let challenge = tokio::task::spawn_blocking(move || {
            let now = unix_now();
            let mut service = DuelApplicationService::with_evolution(
                repository,
                move || now as f64,
                RepositoryDuelEvolution(evolution),
            );
            service.issue(
                guild_id,
                channel_id,
                actor_id,
                recipient_id,
                wager,
                recipient_is_bot,
            )
        })
        .await
        .map_err(|error| format!("duel issue task failed: {error}"))?;
        let challenge = match challenge {
            Ok(challenge) => challenge,
            Err(CommandError::RecipientFunding { wager, .. }) => {
                return followup_silent(
                    &responder,
                    format!(
                        "The duel from <@{actor_id}> to <@{recipient_id}> failed because the challenged player cannot cover the {wager} JC wager."
                    ),
                    false,
                )
                .await;
            }
            Err(error) => return followup_silent(&responder, error.to_string(), true).await,
        };

        let flavor = self.flavor.generate(FlavorEvent::Issued, &challenge).await;
        let response = InteractionResponse::message(format!("<@{recipient_id}>"))
            .with_user_mentions(vec![unsigned_id(recipient_id, "recipient")?])
            .embed(challenge_embed(&challenge, &flavor));
        let receipt = match responder.followup_with_receipt(response).await {
            Ok(Some(receipt)) => receipt,
            Ok(None) | Err(_) => {
                self.refund_failed_delivery(&challenge, guild_id, actor_id, &responder)
                    .await?;
                return Ok(());
            }
        };

        let bound = self
            .bind_challenge(challenge.clone(), guild_id, receipt)
            .await;
        if let Err(error) = bound {
            let _ = self
                .discord
                .delete_message(receipt.channel_id, receipt.message_id)
                .await;
            if !matches!(error, CommandError::Invalid(_)) {
                self.refund_failed_delivery(&challenge, guild_id, actor_id, &responder)
                    .await?;
            }
            return Ok(());
        }

        let controls = InteractionResponse::message("")
            .embed(challenge_embed(&challenge, &flavor))
            .action_row(challenge_buttons(challenge.challenge_id));
        let _ = self
            .discord
            .edit_message(
                receipt.channel_id,
                receipt.message_id,
                DiscordMessage::silent(controls).preserving_content(),
            )
            .await;
        Ok(())
    }

    async fn bind_challenge(
        &self,
        challenge: DuelChallenge,
        guild_id: i64,
        receipt: InteractionMessageReceipt,
    ) -> Result<DuelChallenge, CommandError> {
        let repository = self.repository.clone();
        let evolution = self.evolution.clone();
        tokio::task::spawn_blocking(move || {
            let now = unix_now();
            let mut service = DuelApplicationService::with_evolution(
                repository,
                move || now as f64,
                RepositoryDuelEvolution(evolution),
            );
            service.bind_message(
                challenge.challenge_id,
                guild_id,
                i64::try_from(receipt.message_id).map_err(|_| {
                    CommandError::Invalid("Discord message ID exceeds SQLite range".to_owned())
                })?,
            )
        })
        .await
        .map_err(|error| CommandError::Internal(format!("duel bind task failed: {error}")))?
    }

    async fn refund_failed_delivery(
        &self,
        challenge: &DuelChallenge,
        guild_id: i64,
        actor_id: i64,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let repository = self.repository.clone();
        let evolution = self.evolution.clone();
        let challenge_id = challenge.challenge_id;
        let issuance_fee = challenge.issuance_fee;
        let refunded = tokio::task::spawn_blocking(move || {
            let now = unix_now();
            let mut service = DuelApplicationService::with_evolution(
                repository,
                move || now as f64,
                RepositoryDuelEvolution(evolution),
            );
            service.mark_delivery_failed(challenge_id, guild_id, actor_id)
        })
        .await
        .map_err(|error| format!("duel delivery-refund task failed: {error}"))?;
        match refunded {
            Ok(_) => {
                followup_silent(
                    responder,
                    format!(
                        "The challenge could not be delivered; the wager and {issuance_fee} JC issuance fee were refunded."
                    ),
                    true,
                )
                .await
            }
            Err(CommandError::Invalid(_)) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    async fn respond(
        &self,
        context: DuelCommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let choice = string_option(options, "choice")
            .ok_or_else(|| "/duel respond requires choice".to_owned())?;
        self.respond_choice(context, choice, None, responder).await
    }

    async fn respond_choice(
        &self,
        context: DuelCommandContext,
        choice: &str,
        challenge_id: Option<i64>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some(guild_id) = context.guild_id else {
            return respond_ephemeral(&responder, "This command must be used in a server.").await;
        };
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let repository = self.repository.clone();
        let evolution = self.evolution.clone();
        let recipient_id = context.user_id;
        let choice = choice.to_owned();
        let challenge = tokio::task::spawn_blocking(move || {
            let now = unix_now();
            let mut service = DuelApplicationService::with_evolution(
                repository,
                move || now as f64,
                RepositoryDuelEvolution(evolution),
            );
            service.respond_to(guild_id, recipient_id, &choice, challenge_id)
        })
        .await
        .map_err(|error| format!("duel response task failed: {error}"))?;
        let challenge = match challenge {
            Ok(challenge) => challenge,
            Err(error) => return followup_silent(&responder, error.to_string(), true).await,
        };
        let event = match challenge.status {
            DuelStatus::Declined => FlavorEvent::Declined,
            DuelStatus::Voided => FlavorEvent::Voided,
            _ => match challenge.trial_type {
                Some(DuelTrial::TrialByCombat) => FlavorEvent::AcceptedCombat,
                Some(DuelTrial::TrialOfFive) | None => FlavorEvent::AcceptedFive,
            },
        };
        let flavor = self.flavor.generate(event, &challenge).await;
        self.edit_challenge(&challenge, &flavor).await;
        let detail = if challenge.status == DuelStatus::Declined {
            format!(
                "Challenge #{} was declined. The {} JC penalty was paid to the challenger.",
                challenge.challenge_id,
                challenge.decline_penalty()
            )
        } else if challenge.status == DuelStatus::Voided {
            format!(
                "Challenge #{} was voided because the recipient could not fund the {} JC wager. The challenger's {} JC stake was refunded; the {} JC issuance fee remains nonrefundable after delivery.",
                challenge.challenge_id, challenge.wager, challenge.wager, challenge.issuance_fee
            )
        } else {
            let trial = challenge
                .trial_type
                .map_or("A valid duel trial is required.", |trial| match trial {
                    DuelTrial::TrialByCombat => {
                        "Trial by Combat: best-of-three, one-versus-one Dota mid."
                    }
                    DuelTrial::TrialOfFive => "Trial of Five: a lobby using Immortal Draft.",
                });
            let mut detail = format!(
                "Challenge #{}: <@{}> vs <@{}>. Both {} JC stakes are locked. {trial}",
                challenge.challenge_id,
                challenge.challenger_id,
                challenge.recipient_id,
                challenge.wager
            );
            if challenge.trial_type == Some(DuelTrial::TrialByCombat) {
                detail.push('\n');
                detail.push_str(TRIAL_BY_COMBAT_RULES);
            }
            detail
        };
        followup_silent(&responder, format!("{flavor}\n{detail}"), false).await
    }

    async fn list(
        &self,
        context: DuelCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some(guild_id) = context.guild_id else {
            return respond_ephemeral(&responder, "This command must be used in a server.").await;
        };
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let repository = self.repository.clone();
        let challenges = tokio::task::spawn_blocking(move || {
            repository
                .list_outstanding(Some(guild_id))
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("duel list task failed: {error}"))??;
        if challenges.is_empty() {
            return followup_silent(
                &responder,
                "No pending or accepted duel challenges were found.",
                true,
            )
            .await;
        }
        let pages = challenges.len().div_ceil(25);
        for (index, page) in challenges.chunks(25).enumerate() {
            let mut embed = InteractionEmbed::titled("Outstanding Duel Challenges").color(0xF1C40F);
            if pages > 1 {
                embed = embed.footer(format!("Page {} of {pages}", index + 1));
            }
            for challenge in page {
                let trial = challenge.trial_type.map_or_else(
                    || format!(" • responds <t:{}:R>", challenge.expires_at),
                    |trial| format!(" • {}", trial_label(trial)),
                );
                embed = embed.field(
                    format!(
                        "#{} • {}",
                        challenge.challenge_id,
                        status_label(challenge.status)
                    ),
                    format!(
                        "<@{}> vs <@{}> • {} JC{trial}",
                        challenge.challenger_id, challenge.recipient_id, challenge.wager
                    ),
                    false,
                );
            }
            responder
                .followup(
                    InteractionResponse::message("")
                        .without_mentions()
                        .embed(embed),
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    async fn resolve(
        &self,
        context: DuelCommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some(guild_id) = context.guild_id else {
            return respond_ephemeral(&responder, "This command must be used in a server.").await;
        };
        if !self.admin_user_ids.contains(&context.user_id)
            && !context.member_permissions.is_some_and(|permissions| {
                permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0
            })
        {
            return respond_ephemeral(
                &responder,
                "You do not have permission to resolve duel challenges.",
            )
            .await;
        }
        let challenge_id = integer_option(options, "challenge_id")
            .ok_or_else(|| "/duel resolve requires challenge_id".to_owned())?;
        let outcome = string_option(options, "outcome")
            .ok_or_else(|| "/duel resolve requires outcome".to_owned())?
            .to_owned();
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let repository = self.repository.clone();
        let evolution = self.evolution.clone();
        let actor_id = context.user_id;
        let challenge = tokio::task::spawn_blocking(move || {
            let now = unix_now();
            let mut service = DuelApplicationService::with_evolution(
                repository,
                move || now as f64,
                RepositoryDuelEvolution(evolution),
            );
            service.resolve(guild_id, actor_id, challenge_id, &outcome)
        })
        .await
        .map_err(|error| format!("duel resolution task failed: {error}"))?;
        let challenge = match challenge {
            Ok(challenge) => challenge,
            Err(error) => return followup_silent(&responder, error.to_string(), true).await,
        };
        let event = if challenge.status == DuelStatus::Voided {
            FlavorEvent::Voided
        } else {
            FlavorEvent::Resolved
        };
        let flavor = self.flavor.generate(event, &challenge).await;
        self.edit_challenge(&challenge, &flavor).await;
        let detail = if challenge.status == DuelStatus::Voided {
            format!(
                "Challenge #{} was voided. The {} JC stakes were refunded to each player; the issuance fee was not refunded.",
                challenge.challenge_id, challenge.wager
            )
        } else {
            format!(
                "Challenge #{} is resolved: <@{}> wins {} JC.",
                challenge.challenge_id,
                challenge.winner_id.unwrap_or_default(),
                challenge.wager * 2
            )
        };
        followup_silent(&responder, format!("{flavor}\n{detail}"), false).await
    }

    async fn edit_challenge(&self, challenge: &DuelChallenge, flavor: &str) {
        let (Ok(channel_id), Some(message_id)) = (
            unsigned_id(challenge.channel_id, "channel"),
            challenge.message_id,
        ) else {
            return;
        };
        let Ok(message_id) = unsigned_id(message_id, "message") else {
            return;
        };
        let response = InteractionResponse::message("")
            .without_mentions()
            .embed(challenge_embed(challenge, flavor));
        let _ = self
            .discord
            .edit_message(
                channel_id,
                message_id,
                DiscordMessage::silent(response).preserving_content(),
            )
            .await;
    }
}

fn duel_subcommands() -> Vec<CommandOptionSpec> {
    let mut wager = CommandOptionSpec::new(
        "wager",
        "Wager per duelist (500-1000 JC)",
        CommandOptionKind::Integer,
    )
    .required(true);
    wager.min_integer = Some(500);
    wager.max_integer = Some(1_000);
    vec![
        CommandOptionSpec::new(
            "issue",
            "Challenge a player to a duel of honor",
            CommandOptionKind::Subcommand,
        )
        .options(vec![
            CommandOptionSpec::new("player", "Player to challenge", CommandOptionKind::User)
                .required(true),
            wager,
        ]),
        CommandOptionSpec::new(
            "respond",
            "Answer your pending duel challenge",
            CommandOptionKind::Subcommand,
        )
        .options(vec![choice_option(
            "choice",
            "Your answer",
            &[
                ("Decline in Cowardice", "decline"),
                ("Trial by Combat", "trial_by_combat"),
                ("Trial of Five", "trial_of_five"),
            ],
        )]),
        CommandOptionSpec::new(
            "list",
            "List outstanding duel challenges",
            CommandOptionKind::Subcommand,
        ),
        CommandOptionSpec::new(
            "resolve",
            "Resolve an accepted duel challenge",
            CommandOptionKind::Subcommand,
        )
        .options(vec![
            CommandOptionSpec::new("challenge_id", "Challenge ID", CommandOptionKind::Integer)
                .required(true),
            choice_option(
                "outcome",
                "Duel outcome",
                &[
                    ("Challenger Victory", "challenger_victory"),
                    ("Recipient Victory", "recipient_victory"),
                    ("Void", "void"),
                ],
            ),
        ]),
    ]
}

fn choice_option(name: &str, description: &str, values: &[(&str, &str)]) -> CommandOptionSpec {
    let mut option =
        CommandOptionSpec::new(name, description, CommandOptionKind::String).required(true);
    option.choices = values
        .iter()
        .map(|(label, value)| CommandOptionChoice::String {
            name: (*label).to_owned(),
            value: (*value).to_owned(),
        })
        .collect();
    option
}

fn challenge_buttons(challenge_id: i64) -> InteractionActionRow {
    InteractionActionRow::buttons(vec![
        InteractionButton::new(
            format!("duel:{challenge_id}:decline"),
            "Decline in Cowardice",
        )
        .style(InteractionButtonStyle::Danger)
        .emoji("🏳️"),
        InteractionButton::new(
            format!("duel:{challenge_id}:trial_by_combat"),
            "Trial by Combat",
        )
        .style(InteractionButtonStyle::Primary)
        .emoji("⚔️"),
        InteractionButton::new(
            format!("duel:{challenge_id}:trial_of_five"),
            "Trial of Five",
        )
        .style(InteractionButtonStyle::Success)
        .emoji("🛡️"),
    ])
}

fn challenge_embed(challenge: &DuelChallenge, flavor: &str) -> InteractionEmbed {
    let color = match challenge.status {
        DuelStatus::Pending => 0xF1C40F,
        DuelStatus::Accepted => 0x3498DB,
        DuelStatus::Declined => 0xE74C3C,
        DuelStatus::Expired | DuelStatus::Voided | DuelStatus::DeliveryFailed => 0x607D8B,
        DuelStatus::Resolved => 0x2ECC71,
    };
    let mut embed = InteractionEmbed::titled("Challenge of Honor")
        .description(flavor)
        .color(color)
        .field("Challenge", format!("#{}", challenge.challenge_id), true)
        .field("Status", status_label(challenge.status), true)
        .field("Wager", format!("{} JC", challenge.wager), true)
        .field(
            "Issuance Fee",
            format!(
                "{} JC — nonrefundable after delivery",
                challenge.issuance_fee
            ),
            true,
        )
        .field(
            "Challenger",
            format!("<@{}>", challenge.challenger_id),
            true,
        )
        .field(
            "Challenger Rating",
            format!(
                "{:.0} ± {:.0}",
                challenge.challenger_glicko, challenge.challenger_rd
            ),
            true,
        )
        .field("Recipient", format!("<@{}>", challenge.recipient_id), true)
        .field(
            "Recipient Rating",
            format!(
                "{:.0} ± {:.0}",
                challenge.recipient_glicko, challenge.recipient_rd
            ),
            true,
        )
        .field(
            "Decline Penalty",
            format!("{} JC", challenge.decline_penalty()),
            true,
        );
    if challenge.status == DuelStatus::Pending {
        embed = embed
            .field(
                "Response Deadline",
                format!("<t:{}:R>", challenge.expires_at),
                true,
            )
            .field(
                "Trials",
                "Trial by Combat — best-of-three, one-versus-one Dota mid\nTrial of Five — a lobby using Immortal Draft",
                false,
            );
    }
    if let Some(trial) = challenge.trial_type {
        embed = embed.field("Trial", trial_label(trial), true);
        if challenge.status == DuelStatus::Accepted && trial == DuelTrial::TrialByCombat {
            embed = embed.field("Rules", TRIAL_BY_COMBAT_RULES, false);
        }
    }
    if challenge.status == DuelStatus::Resolved {
        embed = embed.field(
            "Winner",
            format!("<@{}>", challenge.winner_id.unwrap_or_default()),
            true,
        );
    }
    if challenge.status == DuelStatus::Voided {
        let refund = if challenge.trial_type.is_none() {
            embed = embed.field(
                "Funding Failure",
                format!(
                    "The recipient could not fund the {} JC wager.",
                    challenge.wager
                ),
                false,
            );
            format!(
                "{} JC challenger stake refunded; {} JC issuance fee remains nonrefundable after delivery",
                challenge.wager, challenge.issuance_fee
            )
        } else {
            format!(
                "{} JC stake to each player; issuance fee remains nonrefundable",
                challenge.wager
            )
        };
        embed = embed.field("Refund", refund, true);
    }
    embed
}

const fn status_label(status: DuelStatus) -> &'static str {
    match status {
        DuelStatus::Pending => "Pending",
        DuelStatus::Accepted => "Accepted",
        DuelStatus::Declined => "Declined",
        DuelStatus::Expired => "Expired",
        DuelStatus::Resolved => "Resolved",
        DuelStatus::Voided => "Voided",
        DuelStatus::DeliveryFailed => "Delivery Failed",
    }
}

const fn trial_label(trial: DuelTrial) -> &'static str {
    match trial {
        DuelTrial::TrialByCombat => "Trial by Combat",
        DuelTrial::TrialOfFive => "Trial of Five",
    }
}

fn leaf_command(options: &[InteractionOption]) -> Result<(String, &[InteractionOption]), String> {
    options
        .iter()
        .find_map(|option| match &option.value {
            InteractionValue::Subcommand(children) => {
                Some((option.name.clone(), children.as_slice()))
            }
            _ => None,
        })
        .ok_or_else(|| "/duel requires a subcommand".to_owned())
}

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: impl Into<String>,
) -> Result<(), String> {
    responder
        .respond(
            InteractionResponse::message(content)
                .ephemeral()
                .without_mentions(),
        )
        .await
        .map_err(|error| error.to_string())
}

async fn followup_silent(
    responder: &Arc<dyn InteractionResponder>,
    content: impl Into<String>,
    ephemeral: bool,
) -> Result<(), String> {
    let mut response = InteractionResponse::message(content).without_mentions();
    if ephemeral {
        response = response.ephemeral();
    }
    responder
        .followup(response)
        .await
        .map_err(|error| error.to_string())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn signed_id(id: u64, kind: &str) -> Result<i64, String> {
    i64::try_from(id).map_err(|_| format!("Discord {kind} ID exceeds SQLite range"))
}

fn unsigned_id(id: i64, kind: &str) -> Result<u64, String> {
    u64::try_from(id).map_err(|_| format!("SQLite {kind} ID is negative"))
}

#[cfg(all(test, feature = "runtime-test-draft"))]
#[path = "duel_provider/tests.rs"]
mod tests;
