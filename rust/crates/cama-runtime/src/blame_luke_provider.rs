//! Live `/blameluke` launcher and paid persistent investigation button.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use cama_app::blame_luke_media::{BLAME_LUKE_REASONS, render_blame_luke};
use cama_db::blame_luke::{BlameLukeChargeOutcome, BlameLukeRepository};
use tracing::{error, warn};

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
    fn charge(
        &self,
        user_id: i64,
        guild_id: i64,
        selected_reason_index: usize,
    ) -> Result<BlameLukeChargeOutcome, String>;

    fn refund(&self, user_id: i64, guild_id: i64) -> Result<(), String>;
}

impl BlameLukeWalletPort for BlameLukeRepository {
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
                custom_id,
                user_id,
                user_display_name,
                guild_id,
                ..
            } if custom_id == BLAME_LUKE_COMPONENT_ID => {
                self.investigate(user_id, &user_display_name, guild_id, responder)
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
        user_id: u64,
        user_display_name: &str,
        guild_id: Option<u64>,
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
        let selected_index = self.selector.select(BLAME_LUKE_REASONS.len());
        let Some(reason) = BLAME_LUKE_REASONS.get(selected_index).copied() else {
            return Err("Blame Luke reason selector returned an out-of-range index".into());
        };

        let wallet = Arc::clone(&self.wallet);
        let charge =
            tokio::task::spawn_blocking(move || wallet.charge(user_id, guild_id, selected_index))
                .await;
        let charge = match charge {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(message)) => {
                error!(user_id, guild_id, %message, "failed to charge Blame Luke clicker");
                return respond_ephemeral(&responder, WALLET_ERROR).await;
            }
            Err(join_error) => {
                error!(user_id, guild_id, %join_error, "Blame Luke charge task failed");
                return respond_ephemeral(&responder, WALLET_ERROR).await;
            }
        };
        match charge {
            BlameLukeChargeOutcome::Unregistered => {
                return respond_ephemeral(&responder, UNREGISTERED).await;
            }
            BlameLukeChargeOutcome::InsufficientFunds => {
                return respond_ephemeral(&responder, INSUFFICIENT).await;
            }
            BlameLukeChargeOutcome::Charged => {}
        }

        if let Err(response_error) = responder.defer_thinking(false).await {
            warn!(user_id, guild_id, %response_error, "unable to defer Blame Luke interaction");
            self.refund(user_id, guild_id).await;
            return Ok(());
        }

        let renderer = Arc::clone(&self.renderer);
        let rendered = tokio::task::spawn_blocking(move || renderer.render(selected_index)).await;
        let bytes = match rendered {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(message)) => {
                error!(user_id, guild_id, %message, "failed to render Blame Luke apparatus");
                self.refund(user_id, guild_id).await;
                return responder
                    .followup(InteractionResponse::message(RENDER_ERROR))
                    .await
                    .map_err(|error| error.to_string().into());
            }
            Err(join_error) => {
                error!(user_id, guild_id, %join_error, "Blame Luke render task failed");
                self.refund(user_id, guild_id).await;
                return responder
                    .followup(InteractionResponse::message(RENDER_ERROR))
                    .await
                    .map_err(|error| error.to_string().into());
            }
        };

        let response = InteractionResponse::message("")
            .without_mentions()
            .embed(
                InteractionEmbed::titled("📌 Finding of the Ministry of Accountability")
                    .description(format!("**{reason}**"))
                    .color(0xc4_30_36)
                    .image("attachment://blame_luke.gif")
                    .footer(format!("Filed by {user_display_name}")),
            )
            .attachment(InteractionAttachment::bytes("blame_luke.gif", bytes));
        if let Err(response_error) = responder.followup(response).await {
            error!(user_id, guild_id, %response_error, "failed to deliver Blame Luke result");
            self.refund(user_id, guild_id).await;
        }
        Ok(())
    }

    async fn refund(&self, user_id: i64, guild_id: i64) {
        let wallet = Arc::clone(&self.wallet);
        match tokio::task::spawn_blocking(move || wallet.refund(user_id, guild_id)).await {
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

fn signed_id(value: u64, kind: &str) -> Result<i64, InteractionHandlerError> {
    i64::try_from(value)
        .map_err(|_| format!("Discord {kind} ID exceeds SQLite's signed integer range").into())
}

#[cfg(test)]
mod tests;
