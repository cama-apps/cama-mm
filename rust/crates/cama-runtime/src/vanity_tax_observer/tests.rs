use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cama_app::service_container::{ServiceContainer, ServiceContainerOptions};
use cama_app::vanity_tax_service::VanityEligibility;
use rusqlite::Connection;
use tempfile::TempDir;
use tokio::sync::{Notify, oneshot};

use super::*;
use crate::gateway_events::{GatewayEventObservers, GuildMemberPageSource};

#[derive(Debug, Default)]
struct MockMemberHttp {
    members: BTreeMap<u64, Vec<GatewayMember>>,
    calls: Mutex<Vec<(u64, Option<u64>, u64)>>,
}

impl MockMemberHttp {
    fn with_guild(mut self, guild_id: u64, members: Vec<GatewayMember>) -> Self {
        self.members.insert(guild_id, members);
        self
    }

    fn calls(&self) -> Vec<(u64, Option<u64>, u64)> {
        self.calls.lock().expect("calls mutex poisoned").clone()
    }
}

#[async_trait]
impl GuildMemberPageSource for MockMemberHttp {
    async fn fetch_page(
        &self,
        guild_id: u64,
        after: Option<u64>,
        limit: u64,
    ) -> Result<Vec<GatewayMember>, String> {
        self.calls
            .lock()
            .expect("calls mutex poisoned")
            .push((guild_id, after, limit));
        let limit = usize::try_from(limit).map_err(|error| error.to_string())?;
        Ok(self
            .members
            .get(&guild_id)
            .into_iter()
            .flatten()
            .filter(|member| after.is_none_or(|cursor| member.user_id > cursor))
            .take(limit)
            .cloned()
            .collect())
    }
}

fn persistent_service(
    database_path: &Path,
) -> Arc<cama_app::service_container::PersistentVanityTaxService> {
    let connection = Connection::open(database_path).expect("open vanity fixture");
    connection
        .execute_batch(
            "CREATE TABLE vanity_tax_enforcements (
                guild_id INTEGER NOT NULL DEFAULT 0,
                discord_id INTEGER NOT NULL,
                enforced_by INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (guild_id, discord_id)
            );",
        )
        .expect("create vanity fixture schema");
    drop(connection);
    let mut container = ServiceContainer::new(database_path, ServiceContainerOptions::default());
    container.initialize();
    Arc::clone(
        &container
            .components()
            .expect("initialized components")
            .vanity_tax_service,
    )
}

#[tokio::test]
async fn mocked_http_ready_recovery_paginates_and_uses_only_server_nickname() {
    let directory = TempDir::new().expect("temporary directory");
    let database_path = directory.path().join("runtime.sqlite3");
    let service = persistent_service(&database_path);
    service
        .set_manual_taxation(42, 9, true, 500)
        .expect("persist manual enforcement");
    let source = Arc::new(
        MockMemberHttp::default()
            .with_guild(
                42,
                vec![
                    GatewayMember::new(42, 7, None),
                    GatewayMember::new(42, 8, Some("Server Nick".to_owned())),
                    // A global display name is deliberately not part of this
                    // transport type, so user 9 remains nickname-taxable.
                    GatewayMember::new(42, 9, None),
                ],
            )
            .with_guild(84, vec![GatewayMember::new(84, 10, None)]),
    );
    let observer: Arc<dyn GatewayEventObserver> = Arc::new(
        VanityTaxGatewayObserver::with_page_limit(Arc::clone(&service), 2),
    );
    let observers = GatewayEventObservers::new(vec![observer]);
    let reports = observers
        .ready_recovery(ReadyRecoveryContext::new(
            Arc::<[u64]>::from([42, 84]),
            source.clone(),
        ))
        .await;

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].guilds_attempted, 2);
    assert_eq!(reports[0].guilds_refreshed, 2);
    assert_eq!(reports[0].members_refreshed, 4);
    assert!(reports[0].failures.is_empty());
    assert_eq!(
        source.calls(),
        vec![(42, None, 2), (42, Some(8), 2), (84, None, 2),]
    );
    assert_eq!(
        service.eligibility_status(42, 7),
        VanityEligibility::Taxable
    );
    assert_eq!(
        service.eligibility_status(42, 8),
        VanityEligibility::NicknameExemption
    );
    assert_eq!(
        service.eligibility_status(42, 9),
        VanityEligibility::ManualTaxation
    );
    assert_eq!(
        service.eligibility_status(84, 10),
        VanityEligibility::Taxable
    );
}

#[tokio::test]
async fn live_member_events_update_and_remove_the_persistent_service_cache() {
    let directory = TempDir::new().expect("temporary directory");
    let service = persistent_service(&directory.path().join("runtime.sqlite3"));
    let observer: Arc<dyn GatewayEventObserver> =
        Arc::new(VanityTaxGatewayObserver::new(Arc::clone(&service)));
    let observers = GatewayEventObservers::new(vec![observer]);

    assert!(
        observers
            .member_join(GatewayMember::new(42, 7, None))
            .await
            .is_empty()
    );
    assert_eq!(
        service.eligibility_status(42, 7),
        VanityEligibility::Taxable
    );

    assert!(
        observers
            .member_update(GatewayMember::new(42, 7, Some("Real Name".to_owned())))
            .await
            .is_empty()
    );
    assert_eq!(
        service.eligibility_status(42, 7),
        VanityEligibility::NicknameExemption
    );

    assert!(observers.member_remove(42, 7).await.is_empty());
    assert_eq!(
        service.eligibility_status(42, 7),
        VanityEligibility::Unknown
    );
}

#[derive(Debug)]
struct PausedMemberHttp {
    started: Mutex<Option<oneshot::Sender<()>>>,
    release: Arc<Notify>,
}

#[async_trait]
impl GuildMemberPageSource for PausedMemberHttp {
    async fn fetch_page(
        &self,
        guild_id: u64,
        _after: Option<u64>,
        _limit: u64,
    ) -> Result<Vec<GatewayMember>, String> {
        if let Some(started) = self.started.lock().expect("started mutex poisoned").take() {
            let _ = started.send(());
        }
        self.release.notified().await;
        Ok(vec![GatewayMember::new(guild_id, 7, None)])
    }
}

#[tokio::test]
async fn event_delivered_during_http_snapshot_wins_over_stale_ready_data() {
    let directory = TempDir::new().expect("temporary directory");
    let service = persistent_service(&directory.path().join("runtime.sqlite3"));
    let (started_send, started_receive) = oneshot::channel();
    let release = Arc::new(Notify::new());
    let source: Arc<dyn GuildMemberPageSource> = Arc::new(PausedMemberHttp {
        started: Mutex::new(Some(started_send)),
        release: Arc::clone(&release),
    });
    let observer: Arc<dyn GatewayEventObserver> =
        Arc::new(VanityTaxGatewayObserver::new(Arc::clone(&service)));
    let observers = GatewayEventObservers::new(vec![observer]);
    let recovery_observers = observers.clone();
    let recovery = tokio::spawn(async move {
        recovery_observers
            .ready_recovery(ReadyRecoveryContext::new(Arc::<[u64]>::from([42]), source))
            .await
    });

    started_receive.await.expect("HTTP fetch started");
    assert!(
        observers
            .member_update(GatewayMember::new(42, 7, Some("Newest Nick".to_owned())))
            .await
            .is_empty()
    );
    release.notify_waiters();

    let reports = recovery.await.expect("recovery task");
    assert_eq!(reports[0].guilds_refreshed, 1);
    assert_eq!(
        service.eligibility_status(42, 7),
        VanityEligibility::NicknameExemption
    );
}

#[tokio::test]
async fn unsigned_snowflakes_that_do_not_fit_sqlite_are_rejected() {
    let directory = TempDir::new().expect("temporary directory");
    let service = persistent_service(&directory.path().join("runtime.sqlite3"));
    let observer: Arc<dyn GatewayEventObserver> =
        Arc::new(VanityTaxGatewayObserver::new(Arc::clone(&service)));
    let observers = GatewayEventObservers::new(vec![observer]);
    let invalid = i64::MAX as u64 + 1;

    let failures = observers
        .member_join(GatewayMember::new(42, invalid, None))
        .await;
    assert_eq!(failures.len(), 1);
    assert!(failures[0].message.contains("exceeds SQLite INTEGER range"));
    assert!(service.taxable_ids(42).is_empty());

    let source: Arc<dyn GuildMemberPageSource> = Arc::new(MockMemberHttp::default());
    let reports = observers
        .ready_recovery(ReadyRecoveryContext::new(
            Arc::<[u64]>::from([invalid]),
            source,
        ))
        .await;
    assert_eq!(reports[0].failures.len(), 1);
    assert_eq!(reports[0].failures[0].guild_id, invalid);
}
