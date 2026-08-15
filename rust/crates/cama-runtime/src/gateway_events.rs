//! Transport-neutral Discord gateway event observers.
//!
//! The gateway SDK is deliberately kept on the outside of this boundary. A
//! ready-recovery observer receives an explicit, paginated member source, and
//! live member events carry only Discord snowflakes plus the guild-specific
//! nickname. This makes reconnect recovery and cache-maintenance behavior
//! testable without connecting to Discord.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

/// A Discord guild member as observed by the gateway or member-list HTTP API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayMember {
    pub guild_id: u64,
    pub user_id: u64,
    /// The guild-specific nickname. A Discord global display name is
    /// intentionally absent because it must not affect vanity-tax eligibility.
    pub server_nickname: Option<String>,
}

impl GatewayMember {
    #[must_use]
    pub fn new(guild_id: u64, user_id: u64, server_nickname: Option<String>) -> Self {
        Self {
            guild_id,
            user_id,
            server_nickname,
        }
    }
}

/// A page source equivalent to Discord's `GET /guilds/{guild.id}/members`.
#[async_trait]
pub trait GuildMemberPageSource: Send + Sync {
    /// Fetch members strictly after `after`, up to `limit` (Discord caps this
    /// endpoint at 1,000). Implementations must preserve the member's server
    /// nickname rather than substituting a global display name.
    async fn fetch_page(
        &self,
        guild_id: u64,
        after: Option<u64>,
        limit: u64,
    ) -> Result<Vec<GatewayMember>, String>;
}

#[derive(Clone)]
pub struct ReadyRecoveryContext {
    guild_ids: Arc<[u64]>,
    member_source: Arc<dyn GuildMemberPageSource>,
}

impl ReadyRecoveryContext {
    #[must_use]
    pub fn new(
        guild_ids: impl Into<Arc<[u64]>>,
        member_source: Arc<dyn GuildMemberPageSource>,
    ) -> Self {
        Self {
            guild_ids: guild_ids.into(),
            member_source,
        }
    }

    #[must_use]
    pub fn guild_ids(&self) -> &[u64] {
        &self.guild_ids
    }

    #[must_use]
    pub const fn member_source(&self) -> &Arc<dyn GuildMemberPageSource> {
        &self.member_source
    }
}

impl fmt::Debug for ReadyRecoveryContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadyRecoveryContext")
            .field("guild_ids", &self.guild_ids)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadyRecoveryFailure {
    pub guild_id: u64,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadyRecoveryReport {
    pub observer: &'static str,
    pub guilds_attempted: usize,
    pub guilds_refreshed: usize,
    pub guilds_superseded: usize,
    pub members_refreshed: usize,
    pub failures: Vec<ReadyRecoveryFailure>,
}

impl ReadyRecoveryReport {
    #[must_use]
    pub fn empty(observer: &'static str, guilds_attempted: usize) -> Self {
        Self {
            observer,
            guilds_attempted,
            guilds_refreshed: 0,
            guilds_superseded: 0,
            members_refreshed: 0,
            failures: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayObserverFailure {
    pub observer: &'static str,
    pub message: String,
}

/// Observer boundary for live gateway events and full ready/reconnect recovery.
#[async_trait]
pub trait GatewayEventObserver: Send + Sync {
    fn name(&self) -> &'static str;

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport;

    async fn member_join(&self, _member: GatewayMember) -> Result<(), String> {
        Ok(())
    }

    async fn member_update(&self, _member: GatewayMember) -> Result<(), String> {
        Ok(())
    }

    async fn member_remove(&self, _guild_id: u64, _user_id: u64) -> Result<(), String> {
        Ok(())
    }
}

/// Immutable fan-out used by the production Serenity handler.
#[derive(Clone, Default)]
pub struct GatewayEventObservers {
    observers: Arc<[Arc<dyn GatewayEventObserver>]>,
}

impl fmt::Debug for GatewayEventObservers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = self
            .observers
            .iter()
            .map(|observer| observer.name())
            .collect::<Vec<_>>();
        formatter
            .debug_struct("GatewayEventObservers")
            .field("observers", &names)
            .finish()
    }
}

impl GatewayEventObservers {
    #[must_use]
    pub fn new(observers: Vec<Arc<dyn GatewayEventObserver>>) -> Self {
        Self {
            observers: observers.into(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.observers.is_empty()
    }

    pub async fn ready_recovery(&self, context: ReadyRecoveryContext) -> Vec<ReadyRecoveryReport> {
        let mut reports = Vec::with_capacity(self.observers.len());
        for observer in self.observers.iter() {
            reports.push(observer.ready_recovery(context.clone()).await);
        }
        reports
    }

    pub async fn member_join(&self, member: GatewayMember) -> Vec<GatewayObserverFailure> {
        self.dispatch_member(member, MemberEvent::Join).await
    }

    pub async fn member_update(&self, member: GatewayMember) -> Vec<GatewayObserverFailure> {
        self.dispatch_member(member, MemberEvent::Update).await
    }

    pub async fn member_remove(&self, guild_id: u64, user_id: u64) -> Vec<GatewayObserverFailure> {
        let mut failures = Vec::new();
        for observer in self.observers.iter() {
            if let Err(message) = observer.member_remove(guild_id, user_id).await {
                failures.push(GatewayObserverFailure {
                    observer: observer.name(),
                    message,
                });
            }
        }
        failures
    }

    async fn dispatch_member(
        &self,
        member: GatewayMember,
        event: MemberEvent,
    ) -> Vec<GatewayObserverFailure> {
        let mut failures = Vec::new();
        for observer in self.observers.iter() {
            let result = match event {
                MemberEvent::Join => observer.member_join(member.clone()).await,
                MemberEvent::Update => observer.member_update(member.clone()).await,
            };
            if let Err(message) = result {
                failures.push(GatewayObserverFailure {
                    observer: observer.name(),
                    message,
                });
            }
        }
        failures
    }
}

#[derive(Clone, Copy)]
enum MemberEvent {
    Join,
    Update,
}
