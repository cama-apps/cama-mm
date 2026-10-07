//! One durable queue panel per enabled guild, independent of match threads.
use super::*;
use crate::discord_transport::DiscordMessageReceipt;
use crate::gateway_events::{
    GatewayMember, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};

impl DeadlockHandler {
    async fn resolve_lobby_channel(&self, guild: i64, known: Option<u64>) -> Result<u64, String> {
        let channel = self
            .discord
            .resolve_named_text_channel(
                guild as u64,
                self.config.channels.get(&guild).copied(),
                known,
                "deadlock-mm",
            )
            .await?;
        positive_id(channel)?;
        Ok(channel)
    }

    pub(super) async fn lobby_channel(&self, guild: i64) -> Result<u64, String> {
        let _guard = self.lobby_lock.lock().await;
        let repo = DeadlockRepository::new(&self.path);
        let previous =
            blocking(move || repo.lobby_publication(guild).map_err(|e| e.to_string())).await?;
        self.resolve_lobby_channel(guild, previous.map(|p| p.channel_id as u64))
            .await
    }

    pub(super) async fn in_matchmaking_channel(&self, context: &Context) -> Result<bool, String> {
        let Some(channel) = context.channel else {
            return Ok(false);
        };
        let repo = DeadlockRepository::new(&self.path);
        let guild = context.guild;
        let channel_id = positive_id(channel)?;
        let lookup = repo.clone();
        if blocking(move || {
            lookup
                .is_match_channel(guild, channel_id)
                .map_err(|e| e.to_string())
        })
        .await?
        {
            return Ok(true);
        }
        let dedicated = self.lobby_channel(guild).await?;
        if channel == dedicated {
            return Ok(true);
        }
        let parent = self
            .discord
            .channel_parent_id(context.guild as u64, channel)
            .await?;
        if parent == Some(dedicated) {
            return Ok(true);
        }
        let Some(parent) = parent.map(positive_id).transpose()? else {
            return Ok(false);
        };
        blocking(move || {
            repo.is_match_channel(guild, parent)
                .map_err(|e| e.to_string())
        })
        .await
    }

    /// A single publisher lock covers rendering, adoption and receipt persistence.
    /// A lost Discord response is reconciled before another send is attempted.
    pub(super) async fn publish_lobby(
        &self,
        guild: i64,
        format: Option<DeadlockFormat>,
    ) -> Result<DiscordMessageReceipt, String> {
        let _guard = self.lobby_lock.lock().await;
        let repo = DeadlockRepository::new(&self.path);
        let lookup = repo.clone();
        let previous =
            blocking(move || lookup.lobby_publication(guild).map_err(|e| e.to_string())).await?;
        let channel = self
            .resolve_lobby_channel(guild, previous.as_ref().map(|p| p.channel_id as u64))
            .await?;
        if let Some(previous) = &previous
            && previous.channel_id as u64 != channel
        {
            if let Some(message) = previous.message_id
                && self
                    .discord
                    .fetch_message(previous.channel_id as u64, message as u64)
                    .await?
                    .is_some()
            {
                self.discord
                    .delete_message(previous.channel_id as u64, message as u64)
                    .await?;
            }
            self.lobby_rendered
                .lock()
                .map_err(|_| "Lobby display cache unavailable")?
                .remove(&guild);
        }
        let prepare = repo.clone();
        let mut publication = blocking(move || {
            prepare
                .prepare_lobby_publication(guild, channel as i64, format, now())
                .map_err(|e| e.to_string())
        })
        .await?;
        let mut receipt = None;
        if let Some(message) = publication.message_id {
            // Errors (permissions, rate limits, outages) never imply deletion.
            if let Some(existing) = self.discord.fetch_message(channel, message as u64).await? {
                receipt = Some(existing.receipt);
            } else {
                let replace = repo.clone();
                let missing = publication.clone();
                publication = blocking(move || {
                    replace
                        .replace_missing_lobby_message(guild, &missing, now())
                        .map_err(|e| e.to_string())?;
                    replace
                        .prepare_lobby_publication(guild, channel as i64, format, now())
                        .map_err(|e| e.to_string())
                })
                .await?;
                self.lobby_rendered
                    .lock()
                    .map_err(|_| "Lobby display cache unavailable")?
                    .remove(&guild);
            }
        }
        let response = self.queue_response(guild, publication.format).await?;
        let cached = self
            .lobby_rendered
            .lock()
            .map_err(|_| "Lobby display cache unavailable")?
            .get(&guild)
            .cloned();
        let first_reconciliation = receipt.as_ref().is_none_or(|receipt| {
            cached
                .as_ref()
                .is_none_or(|(id, _)| *id != receipt.message_id)
        });
        let legacy = if first_reconciliation {
            self.discord.find_deadlock_lobby_messages(channel).await?
        } else {
            Vec::new()
        };
        if receipt.is_none() {
            receipt = legacy.first().cloned();
        }
        let key = format!("deadlock:{guild}:lobby:{}", publication.generation);
        if receipt.is_none() {
            receipt = self
                .discord
                .find_message_by_delivery_key(channel, &key, publication.created_at, 500)
                .await?;
        }
        let (receipt, sent) = match receipt {
            Some(receipt) => (receipt, false),
            None => (
                self.discord
                    .send_message_with_delivery_key(
                        channel,
                        &key,
                        DiscordMessage::silent(response.clone()),
                    )
                    .await?,
                true,
            ),
        };
        if receipt.channel_id != channel {
            return Err("Lobby receipt belongs to another channel".into());
        }
        if !sent && cached.as_ref() != Some(&(receipt.message_id, response.clone())) {
            self.discord
                .edit_message(
                    channel,
                    receipt.message_id,
                    DiscordMessage::silent(response.clone()),
                )
                .await?;
        }
        let saved = publication.clone();
        let message = positive_id(receipt.message_id)?;
        blocking(move || {
            repo.save_lobby_message(guild, &saved, message, now())
                .map_err(|e| e.to_string())
        })
        .await?;
        // Only positively identified, bot-owned queue panels are removed.
        for duplicate in legacy {
            if duplicate.message_id != receipt.message_id {
                self.discord
                    .delete_message(channel, duplicate.message_id)
                    .await?;
            }
        }
        self.lobby_rendered
            .lock()
            .map_err(|_| "Lobby display cache unavailable")?
            .insert(guild, (receipt.message_id, response));
        Ok(receipt)
    }
}

pub(super) struct DeadlockLobbyObserver {
    pub(super) handler: Arc<DeadlockHandler>,
}
#[async_trait]
impl GatewayEventObserver for DeadlockLobbyObserver {
    fn name(&self) -> &'static str {
        "deadlock-lobby"
    }
    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        let guilds: Vec<_> = context
            .guild_ids()
            .iter()
            .copied()
            .filter(|id| positive_id(*id).is_ok_and(|id| self.handler.config.allows(id)))
            .collect();
        let mut report = ReadyRecoveryReport::empty(self.name(), guilds.len());
        for guild_id in guilds {
            match self.handler.publish_lobby(guild_id as i64, None).await {
                Ok(_) => report.guilds_refreshed += 1,
                Err(message) => report
                    .failures
                    .push(ReadyRecoveryFailure { guild_id, message }),
            }
        }
        report
    }
    async fn member_update(&self, member: GatewayMember) -> Result<(), String> {
        let guild = positive_id(member.guild_id)?;
        if self.handler.config.allows(guild) {
            self.handler.publish_lobby(guild, None).await?;
        }
        Ok(())
    }
    async fn member_remove(&self, guild_id: u64, user_id: u64) -> Result<(), String> {
        let guild = positive_id(guild_id)?;
        if self.handler.config.allows(guild) {
            let user = positive_id(user_id)?;
            let repo = DeadlockRepository::new(&self.handler.path);
            blocking(move || repo.queue_leave(guild, user).map_err(|e| e.to_string())).await?;
            self.handler.publish_lobby(guild, None).await?;
        }
        Ok(())
    }
}
