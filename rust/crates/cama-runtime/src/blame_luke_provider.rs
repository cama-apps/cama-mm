//! Live `/blameluke` launcher and paid persistent investigation button.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use cama_app::blame_luke_media::{BLAME_LUKE_REASONS, render_blame_luke};
use cama_db::blame_luke::{
    BlameLukeChargeOutcome, BlameLukeExistingOperation, BlameLukeOperationIdentity,
    BlameLukeOperationOutcome, BlameLukeRecoverySummary, BlameLukeRepository,
};
use tracing::{error, warn};

use crate::gateway_events::{
    GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};
use crate::registration::{
    CommandSpec, ComponentRoute, InteractionActionRow, InteractionAttachment, InteractionButton,
    InteractionButtonStyle, InteractionEmbed, InteractionHandler, InteractionHandlerError,
    InteractionRequest, InteractionResponder, InteractionResponse, RegistrationError,
    RegistrationProvider, RegistryBuilder,
};

pub const BLAME_LUKE_COMPONENT_ID: &str = "blame_luke:investigate";
const COMPONENT_PREFIX: &str = "blame_luke:";
const GUILD_ONLY_MESSAGE: &str = "This command can only be used in a server.";
const WALLET_ERROR: &str = "The blame apparatus couldn't reach your wallet. You were not charged.";
const UNREGISTERED: &str = "Register first with `/player register` to fund an investigation.";
const INSUFFICIENT: &str = "The Ministry rejected your funding request.";
const RENDER_ERROR: &str =
    "The blame apparatus jammed. Luke is probably responsible. Your payment was refunded.";

#[derive(Clone)]
pub struct BlameLukeRegistrationProvider {
    handler: Arc<BlameLukeHandler>,
}

impl BlameLukeRegistrationProvider {
    /// Compose the complete live provider from the existing migrated SQLite
    /// database, native GIF renderer, and production random source.
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>) -> Self {
        Self::with_ports(
            Arc::new(BlameLukeRepository::new(database_path)),
            Arc::new(RandomReasonSelector),
            Arc::new(NativeBlameLukeRenderer),
        )
    }

    fn with_ports(
        wallet: Arc<dyn BlameLukeWalletPort>,
        selector: Arc<dyn BlameLukeReasonSelector>,
        renderer: Arc<dyn BlameLukeRenderPort>,
    ) -> Self {
        Self {
            handler: Arc::new(BlameLukeHandler {
                wallet,
                selector,
                renderer,
            }),
        }
    }

    #[must_use]
    pub fn gateway_observer(&self) -> Arc<dyn GatewayEventObserver> {
        Arc::new(BlameLukeReadyRecoveryObserver {
            wallet: Arc::clone(&self.handler.wallet),
        })
    }
}

impl RegistrationProvider for BlameLukeRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "blameluke".to_owned(),
            description: "Discover why this is Luke's fault".to_owned(),
            options: Vec::new(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

trait BlameLukeWalletPort: Send + Sync {
    fn eligibility(&self, _user_id: i64, _guild_id: i64) -> Result<BlameLukeChargeOutcome, String> {
        Ok(BlameLukeChargeOutcome::Charged)
    }

    fn charge(
        &self,
        user_id: i64,
        guild_id: i64,
        selected_reason_index: usize,
    ) -> Result<BlameLukeChargeOutcome, String>;

    fn refund(&self, user_id: i64, guild_id: i64) -> Result<(), String>;

    fn charge_for_interaction(
        &self,
        identity: &BlameLukeOperationIdentity,
    ) -> Result<BlameLukeOperationOutcome, String> {
        self.charge(
            identity.user_id,
            identity.guild_id,
            identity.selected_reason_index,
        )
        .map(|outcome| match outcome {
            BlameLukeChargeOutcome::Charged => BlameLukeOperationOutcome::Charged,
            BlameLukeChargeOutcome::Unregistered => BlameLukeOperationOutcome::Unregistered,
            BlameLukeChargeOutcome::InsufficientFunds => {
                BlameLukeOperationOutcome::InsufficientFunds
            }
        })
    }

    fn mark_interaction_delivered(&self, _interaction_id: i64) -> Result<(), String> {
        Ok(())
    }

    fn existing_operation(
        &self,
        _interaction_id: i64,
    ) -> Result<Option<BlameLukeExistingOperation>, String> {
        Ok(None)
    }

    fn refund_interaction(
        &self,
        user_id: i64,
        guild_id: i64,
        _interaction_id: i64,
    ) -> Result<(), String> {
        self.refund(user_id, guild_id)
    }

    fn recover_pending_interactions(&self) -> Result<BlameLukeRecoverySummary, String> {
        Ok(BlameLukeRecoverySummary::default())
    }
}

impl BlameLukeWalletPort for BlameLukeRepository {
    fn eligibility(&self, user_id: i64, guild_id: i64) -> Result<BlameLukeChargeOutcome, String> {
        BlameLukeRepository::eligibility(self, user_id, guild_id).map_err(|error| error.to_string())
    }

    fn charge(
        &self,
        user_id: i64,
        guild_id: i64,
        selected_reason_index: usize,
    ) -> Result<BlameLukeChargeOutcome, String> {
        BlameLukeRepository::charge(self, user_id, guild_id, selected_reason_index)
            .map_err(|error| error.to_string())
    }

    fn refund(&self, user_id: i64, guild_id: i64) -> Result<(), String> {
        BlameLukeRepository::refund(self, user_id, guild_id).map_err(|error| error.to_string())
    }

    fn charge_for_interaction(
        &self,
        identity: &BlameLukeOperationIdentity,
    ) -> Result<BlameLukeOperationOutcome, String> {
        BlameLukeRepository::charge_for_interaction(self, identity)
            .map_err(|error| error.to_string())
    }

    fn mark_interaction_delivered(&self, interaction_id: i64) -> Result<(), String> {
        BlameLukeRepository::mark_interaction_delivered(self, interaction_id)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn existing_operation(
        &self,
        interaction_id: i64,
    ) -> Result<Option<BlameLukeExistingOperation>, String> {
        BlameLukeRepository::existing_operation(self, interaction_id)
            .map_err(|error| error.to_string())
    }

    fn refund_interaction(
        &self,
        _user_id: i64,
        _guild_id: i64,
        interaction_id: i64,
    ) -> Result<(), String> {
        BlameLukeRepository::refund_interaction(self, interaction_id)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn recover_pending_interactions(&self) -> Result<BlameLukeRecoverySummary, String> {
        BlameLukeRepository::recover_pending_interactions(self).map_err(|error| error.to_string())
    }
}

trait BlameLukeReasonSelector: Send + Sync {
    fn select(&self, candidate_count: usize) -> usize;
}

struct RandomReasonSelector;

impl BlameLukeReasonSelector for RandomReasonSelector {
    fn select(&self, candidate_count: usize) -> usize {
        fastrand::usize(..candidate_count)
    }
}

trait BlameLukeRenderPort: Send + Sync {
    fn render(&self, selected_reason_index: usize) -> Result<Vec<u8>, String>;
}

struct NativeBlameLukeRenderer;

impl BlameLukeRenderPort for NativeBlameLukeRenderer {
    fn render(&self, selected_reason_index: usize) -> Result<Vec<u8>, String> {
        render_blame_luke(selected_reason_index)
            .map(|asset| asset.bytes)
            .map_err(|error| error.to_string())
    }
}

struct BlameLukeHandler {
    wallet: Arc<dyn BlameLukeWalletPort>,
    selector: Arc<dyn BlameLukeReasonSelector>,
    renderer: Arc<dyn BlameLukeRenderPort>,
}

struct BlameLukeReadyRecoveryObserver {
    wallet: Arc<dyn BlameLukeWalletPort>,
}

#[async_trait]
impl GatewayEventObserver for BlameLukeReadyRecoveryObserver {
    fn name(&self) -> &'static str {
        "blame_luke_delivery_recovery"
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        let wallet = Arc::clone(&self.wallet);
        match tokio::task::spawn_blocking(move || wallet.recover_pending_interactions()).await {
            Ok(Ok(recovered)) => {
                let mut report = ReadyRecoveryReport::empty(self.name(), context.guild_ids().len());
                report.guilds_refreshed = recovered.guilds_recovered.min(context.guild_ids().len());
                report.members_refreshed = recovered.operations_recovered;
                report
            }
            Ok(Err(message)) => ReadyRecoveryReport {
                observer: self.name(),
                guilds_attempted: context.guild_ids().len(),
                guilds_refreshed: 0,
                guilds_superseded: 0,
                members_refreshed: 0,
                failures: vec![ReadyRecoveryFailure {
                    guild_id: context.guild_ids().first().copied().unwrap_or_default(),
                    message,
                }],
            },
            Err(join_error) => ReadyRecoveryReport {
                observer: self.name(),
                guilds_attempted: context.guild_ids().len(),
                guilds_refreshed: 0,
                guilds_superseded: 0,
                members_refreshed: 0,
                failures: vec![ReadyRecoveryFailure {
                    guild_id: context.guild_ids().first().copied().unwrap_or_default(),
                    message: format!("Blame Luke recovery task failed: {join_error}"),
                }],
            },
        }
    }
}

#[async_trait]
impl InteractionHandler for BlameLukeHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command { name, guild_id, .. } if name == "blameluke" => {
                self.launcher(guild_id, responder).await
            }
            InteractionRequest::Component {
                interaction_id,
                custom_id,
                user_id,
                user_display_name,
                guild_id,
                channel_id,
                ..
            } if custom_id == BLAME_LUKE_COMPONENT_ID => {
                self.investigate(
                    interaction_id,
                    user_id,
                    &user_display_name,
                    guild_id,
                    channel_id,
                    responder,
                )
                .await
            }
            InteractionRequest::Command { name, .. } => {
                Err(format!("Blame Luke handler received command {name:?}").into())
            }
            InteractionRequest::Component { custom_id, .. } => {
                Err(format!("Blame Luke handler received component {custom_id:?}").into())
            }
            InteractionRequest::Autocomplete { .. } | InteractionRequest::Modal { .. } => {
                Err("Blame Luke extension received an unsupported interaction".into())
            }
        }
    }
}

impl BlameLukeHandler {
    async fn launcher(
        &self,
        guild_id: Option<u64>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if guild_id.is_none() {
            return responder
                .respond(InteractionResponse::message(GUILD_ONLY_MESSAGE).ephemeral())
                .await
                .map_err(|error| error.to_string().into());
        }
        let embed = InteractionEmbed::titled("Ministry of Accountability")
            .description(
                "Need a culprit? Press the button and the impartial blame apparatus will investigate.",
            )
            .color(0xd9_a6_2e)
            .footer("Anyone can press it. The clicker pays.");
        let button = InteractionButton::new(BLAME_LUKE_COMPONENT_ID, "Blame Luke")
            .style(InteractionButtonStyle::Danger)
            .emoji("📌");
        responder
            .respond(
                InteractionResponse::message("")
                    .without_mentions()
                    .embed(embed)
                    .action_row(InteractionActionRow::buttons(vec![button])),
            )
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn investigate(
        &self,
        interaction_id: u64,
        user_id: u64,
        user_display_name: &str,
        guild_id: Option<u64>,
        channel_id: Option<u64>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(guild_id) = guild_id else {
            return responder
                .respond(InteractionResponse::message(GUILD_ONLY_MESSAGE).ephemeral())
                .await
                .map_err(|error| error.to_string().into());
        };
        let user_id = signed_id(user_id, "user")?;
        let guild_id = signed_id(guild_id, "guild")?;
        let interaction_id = signed_id(interaction_id, "interaction")?;
        let channel_id = channel_id
            .map(|value| signed_id(value, "channel"))
            .transpose()?;
        let wallet = Arc::clone(&self.wallet);
        let existing = tokio::task::spawn_blocking({
            let wallet = Arc::clone(&wallet);
            move || wallet.existing_operation(interaction_id)
        })
        .await;
        let existing = match existing {
            Ok(Ok(existing)) => existing,
            Ok(Err(message)) => {
                error!(user_id, guild_id, %message, "failed to load Blame Luke operation");
                return respond_ephemeral(&responder, WALLET_ERROR).await;
            }
            Err(join_error) => {
                error!(user_id, guild_id, %join_error, "Blame Luke operation lookup failed");
                return respond_ephemeral(&responder, WALLET_ERROR).await;
            }
        };
        let (identity, pending) = match existing {
            Some(BlameLukeExistingOperation::Delivered)
            | Some(BlameLukeExistingOperation::Refunded) => return Ok(()),
            Some(BlameLukeExistingOperation::Pending(identity)) => {
                if identity.user_id != user_id
                    || identity.guild_id != guild_id
                    || identity.channel_id != channel_id
                    || identity.user_display_name != user_display_name
                {
                    error!(
                        user_id,
                        guild_id,
                        interaction_id,
                        "Blame Luke interaction identity changed during replay"
                    );
                    return respond_ephemeral(&responder, WALLET_ERROR).await;
                }
                (identity, true)
            }
            None => {
                let selected_index = self.selector.select(BLAME_LUKE_REASONS.len());
                (
                    BlameLukeOperationIdentity {
                        interaction_id,
                        user_id,
                        guild_id,
                        channel_id,
                        selected_reason_index: selected_index,
                        user_display_name: user_display_name.to_owned(),
                    },
                    false,
                )
            }
        };
        let user_id = identity.user_id;
        let guild_id = identity.guild_id;
        let Some(reason) = BLAME_LUKE_REASONS
            .get(identity.selected_reason_index)
            .copied()
        else {
            return Err("Blame Luke reason selector returned an out-of-range index".into());
        };

        if !pending {
            let wallet = Arc::clone(&self.wallet);
            let eligibility =
                tokio::task::spawn_blocking(move || wallet.eligibility(user_id, guild_id)).await;
            let eligibility = match eligibility {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(message)) => {
                    error!(user_id, guild_id, %message, "failed to check Blame Luke clicker eligibility");
                    return respond_ephemeral(&responder, WALLET_ERROR).await;
                }
                Err(join_error) => {
                    error!(user_id, guild_id, %join_error, "Blame Luke eligibility task failed");
                    return respond_ephemeral(&responder, WALLET_ERROR).await;
                }
            };
            match eligibility {
                BlameLukeChargeOutcome::Unregistered => {
                    return respond_ephemeral(&responder, UNREGISTERED).await;
                }
                BlameLukeChargeOutcome::InsufficientFunds => {
                    return respond_ephemeral(&responder, INSUFFICIENT).await;
                }
                BlameLukeChargeOutcome::Charged => {}
            }
        }

        // Acknowledge the interaction before the potentially blocking native
        // render. No wallet mutation has happened yet, so an expired defer or
        // a process death here cannot strand a charge.
        if let Err(response_error) = responder.defer_thinking(false).await {
            warn!(user_id, guild_id, %response_error, "unable to defer Blame Luke interaction");
            return Ok(());
        }

        // Render before charging. A renderer crash therefore cannot create a
        // debit; the later durable charge marker covers every network window.
        let renderer = Arc::clone(&self.renderer);
        let rendered =
            tokio::task::spawn_blocking(move || renderer.render(identity.selected_reason_index))
                .await;
        let bytes = match rendered {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(message)) => {
                error!(user_id, guild_id, %message, "failed to render Blame Luke apparatus");
                return responder
                    .followup(InteractionResponse::message(RENDER_ERROR))
                    .await
                    .map_err(|error| error.to_string().into());
            }
            Err(join_error) => {
                error!(user_id, guild_id, %join_error, "Blame Luke render task failed");
                return responder
                    .followup(InteractionResponse::message(RENDER_ERROR))
                    .await
                    .map_err(|error| error.to_string().into());
            }
        };

        let wallet = Arc::clone(&self.wallet);
        let identity_for_charge = identity.clone();
        let charge = tokio::task::spawn_blocking(move || {
            wallet.charge_for_interaction(&identity_for_charge)
        })
        .await;
        let charge = match charge {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(message)) => {
                error!(user_id, guild_id, %message, "failed to charge Blame Luke clicker");
                return followup_ephemeral(&responder, WALLET_ERROR).await;
            }
            Err(join_error) => {
                error!(user_id, guild_id, %join_error, "Blame Luke charge task failed");
                return followup_ephemeral(&responder, WALLET_ERROR).await;
            }
        };
        match charge {
            BlameLukeOperationOutcome::Unregistered => {
                return followup_ephemeral(&responder, UNREGISTERED).await;
            }
            BlameLukeOperationOutcome::InsufficientFunds => {
                return followup_ephemeral(&responder, INSUFFICIENT).await;
            }
            BlameLukeOperationOutcome::Charged | BlameLukeOperationOutcome::AlreadyCharged => {}
            BlameLukeOperationOutcome::AlreadyDelivered
            | BlameLukeOperationOutcome::AlreadyRefunded => return Ok(()),
        }

        let response = InteractionResponse::message("")
            .without_mentions()
            .embed(
                InteractionEmbed::titled("📌 Finding of the Ministry of Accountability")
                    .description(format!("**{reason}**"))
                    .color(0xc4_30_36)
                    .image("attachment://blame_luke.gif")
                    .footer(format!("Filed by {}", identity.user_display_name)),
            )
            .attachment(InteractionAttachment::bytes("blame_luke.gif", bytes));
        if let Err(response_error) = responder.followup(response).await {
            error!(user_id, guild_id, %response_error, "failed to deliver Blame Luke result");
            self.refund(user_id, guild_id, interaction_id).await;
        } else {
            let wallet = Arc::clone(&self.wallet);
            match tokio::task::spawn_blocking(move || {
                wallet.mark_interaction_delivered(interaction_id)
            })
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(message)) => {
                    error!(user_id, guild_id, %message, "failed to record Blame Luke delivery receipt");
                }
                Err(join_error) => {
                    error!(user_id, guild_id, %join_error, "Blame Luke delivery receipt task failed");
                }
            }
        }
        Ok(())
    }

    async fn refund(&self, user_id: i64, guild_id: i64, interaction_id: i64) {
        let wallet = Arc::clone(&self.wallet);
        match tokio::task::spawn_blocking(move || {
            wallet.refund_interaction(user_id, guild_id, interaction_id)
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                error!(user_id, guild_id, %message, "failed to refund Blame Luke clicker");
            }
            Err(join_error) => {
                error!(user_id, guild_id, %join_error, "Blame Luke refund task failed");
            }
        }
    }
}

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: &str,
) -> Result<(), InteractionHandlerError> {
    responder
        .respond(InteractionResponse::message(content).ephemeral())
        .await
        .map_err(|error| error.to_string().into())
}

async fn followup_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: &str,
) -> Result<(), InteractionHandlerError> {
    responder
        .followup(InteractionResponse::message(content).ephemeral())
        .await
        .map_err(|error| error.to_string().into())
}

fn signed_id(value: u64, kind: &str) -> Result<i64, InteractionHandlerError> {
    i64::try_from(value)
        .map_err(|_| format!("Discord {kind} ID exceeds SQLite's signed integer range").into())
}

#[cfg(test)]
mod tests;
