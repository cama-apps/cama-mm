//! Discord gateway adapter for the persistent vanity-tax membership cache.

use std::sync::Arc;

use async_trait::async_trait;
use cama_app::service_container::{PersistentVanityTaxService, VanityMember};

use crate::gateway_events::{
    GatewayEventObserver, GatewayMember, GuildMemberPageSource, ReadyRecoveryContext,
    ReadyRecoveryFailure, ReadyRecoveryReport,
};

pub const DISCORD_MEMBER_PAGE_LIMIT: u64 = 1_000;
const OBSERVER_NAME: &str = "vanity-tax-membership";

#[derive(Debug)]
pub struct VanityTaxGatewayObserver {
    service: Arc<PersistentVanityTaxService>,
    page_limit: u64,
}

impl VanityTaxGatewayObserver {
    #[must_use]
    pub fn new(service: Arc<PersistentVanityTaxService>) -> Self {
        Self {
            service,
            page_limit: DISCORD_MEMBER_PAGE_LIMIT,
        }
    }

    #[cfg(test)]
    fn with_page_limit(service: Arc<PersistentVanityTaxService>, page_limit: u64) -> Self {
        Self {
            service,
            page_limit,
        }
    }

    fn signed_id(kind: &'static str, value: u64) -> Result<i64, String> {
        i64::try_from(value)
            .map_err(|_| format!("Discord {kind} snowflake {value} exceeds SQLite INTEGER range"))
    }

    async fn fetch_guild_members(
        &self,
        source: &dyn GuildMemberPageSource,
        guild_id: u64,
    ) -> Result<Vec<VanityMember>, String> {
        let mut after = None;
        let mut members = Vec::new();
        loop {
            let page = source.fetch_page(guild_id, after, self.page_limit).await?;
            if page.len() > usize::try_from(self.page_limit).unwrap_or(usize::MAX) {
                return Err(format!(
                    "Discord returned {} members for a page limited to {}",
                    page.len(),
                    self.page_limit
                ));
            }

            let page_len = page.len();
            let mut next_after = after;
            for member in page {
                if member.guild_id != guild_id {
                    return Err(format!(
                        "Discord member {} belongs to guild {}, not requested guild {guild_id}",
                        member.user_id, member.guild_id
                    ));
                }
                if let Some(cursor) = after.filter(|&cursor| member.user_id <= cursor) {
                    return Err(format!(
                        "Discord member pagination did not advance after snowflake {cursor}"
                    ));
                }
                next_after =
                    Some(next_after.map_or(member.user_id, |current| current.max(member.user_id)));
                members.push(VanityMember {
                    discord_id: Self::signed_id("user", member.user_id)?,
                    nickname: member.server_nickname,
                });
            }

            if page_len < usize::try_from(self.page_limit).unwrap_or(usize::MAX) {
                break;
            }
            let Some(cursor) = next_after else {
                return Err("Discord returned a full member page without a cursor".to_owned());
            };
            if after == Some(cursor) {
                return Err(format!(
                    "Discord member pagination repeated cursor snowflake {cursor}"
                ));
            }
            after = Some(cursor);
        }
        Ok(members)
    }

    fn update_member(&self, member: GatewayMember) -> Result<(), String> {
        let guild_id = Self::signed_id("guild", member.guild_id)?;
        let user_id = Self::signed_id("user", member.user_id)?;
        self.service
            .update_member(guild_id, user_id, member.server_nickname.as_deref());
        Ok(())
    }
}

#[async_trait]
impl GatewayEventObserver for VanityTaxGatewayObserver {
    fn name(&self) -> &'static str {
        OBSERVER_NAME
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        let mut report = ReadyRecoveryReport::empty(OBSERVER_NAME, context.guild_ids().len());

        // Establish every guild's new authority before issuing an HTTP request.
        // Member events delivered while any page is in flight are then replayed
        // over that snapshot by the service generation journal.
        let refreshes = context
            .guild_ids()
            .iter()
            .map(|&guild_id| {
                let refresh = Self::signed_id("guild", guild_id).map(|signed_guild_id| {
                    let generation = self.service.begin_refresh(signed_guild_id);
                    (guild_id, signed_guild_id, generation)
                });
                (guild_id, refresh)
            })
            .collect::<Vec<_>>();

        for (candidate_guild_id, refresh) in refreshes {
            let (guild_id, signed_guild_id, generation) = match refresh {
                Ok(refresh) => refresh,
                Err(message) => {
                    report.failures.push(ReadyRecoveryFailure {
                        guild_id: candidate_guild_id,
                        message,
                    });
                    continue;
                }
            };
            let members = match self
                .fetch_guild_members(context.member_source().as_ref(), guild_id)
                .await
            {
                Ok(members) => members,
                Err(message) => {
                    report
                        .failures
                        .push(ReadyRecoveryFailure { guild_id, message });
                    continue;
                }
            };
            let member_count = members.len();
            let service = Arc::clone(&self.service);
            let refresh_result = tokio::task::spawn_blocking(move || {
                service.refresh_guild_generation(signed_guild_id, &members, generation)
            })
            .await;
            match refresh_result {
                Ok(Ok(true)) => {
                    report.guilds_refreshed += 1;
                    report.members_refreshed += member_count;
                }
                Ok(Ok(false)) => report.guilds_superseded += 1,
                Ok(Err(error)) => report.failures.push(ReadyRecoveryFailure {
                    guild_id,
                    message: error.to_string(),
                }),
                Err(error) => report.failures.push(ReadyRecoveryFailure {
                    guild_id,
                    message: format!("vanity-tax refresh task failed: {error}"),
                }),
            }
        }
        report
    }

    async fn member_join(&self, member: GatewayMember) -> Result<(), String> {
        self.update_member(member)
    }

    async fn member_update(&self, member: GatewayMember) -> Result<(), String> {
        self.update_member(member)
    }

    async fn member_remove(&self, guild_id: u64, user_id: u64) -> Result<(), String> {
        self.service.remove_member(
            Self::signed_id("guild", guild_id)?,
            Self::signed_id("user", user_id)?,
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "vanity_tax_observer/tests.rs"]
mod tests;
