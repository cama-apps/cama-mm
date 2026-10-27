//! Runtime service composition: resolves the persisted vanity-tax service the
//! production runtime and provider tests share. The remaining runtime
//! services are composed directly in `cama-runtime`.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cama_db::vanity_tax_repository::VanityTaxRepository;
use thiserror::Error;

use crate::ai_services::AIService;
use crate::opendota_http::OpenDotaRuntimeServices;
pub use crate::vanity_tax_service::VanityMember;
use crate::vanity_tax_service::{VanityEligibility, VanityTaxService, VanityTaxServiceError};

static NEXT_MEMORY_DATABASE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct ServiceContainerOptions {
    pub admin_user_ids: Vec<i64>,
    pub lobby_ready_threshold: i64,
    pub lobby_max_players: i64,
    pub use_glicko: bool,
    pub max_debt: i64,
    pub leverage_tiers: Vec<i64>,
    pub garnishment_percentage: f64,
    pub economy_events_enabled: bool,
    pub llm_api_key: Option<String>,
    pub ai_model: String,
    pub ai_timeout_seconds: f64,
    pub ai_max_tokens: i64,
    /// Concrete HTTP-backed service built after database admission. A key
    /// without this value is permitted only for policy-only unit fixtures.
    pub production_ai_service: Option<Arc<AIService>>,
    pub dig_llm_enabled: bool,
    pub pet_enabled: bool,
    pub vanity_tax_rate: f64,
    /// Concrete shared OpenDota transport constructed by the runtime. Tests
    /// that exercise unrelated container policy may leave it absent.
    pub opendota: Option<Arc<OpenDotaRuntimeServices>>,
}

impl Default for ServiceContainerOptions {
    fn default() -> Self {
        Self {
            admin_user_ids: Vec::new(),
            lobby_ready_threshold: 10,
            lobby_max_players: 14,
            use_glicko: true,
            max_debt: 500,
            leverage_tiers: vec![2, 3, 5],
            garnishment_percentage: 1.0,
            economy_events_enabled: false,
            llm_api_key: None,
            ai_model: "groq/qwen/qwen3.6-27b".to_owned(),
            ai_timeout_seconds: 15.0,
            ai_max_tokens: 500,
            production_ai_service: None,
            dig_llm_enabled: true,
            pet_enabled: false,
            vanity_tax_rate: 0.20,
            opendota: None,
        }
    }
}

impl fmt::Debug for ServiceContainerOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServiceContainerOptions")
            .field("admin_user_ids", &self.admin_user_ids)
            .field("lobby_ready_threshold", &self.lobby_ready_threshold)
            .field("lobby_max_players", &self.lobby_max_players)
            .field("use_glicko", &self.use_glicko)
            .field("max_debt", &self.max_debt)
            .field("leverage_tiers", &self.leverage_tiers)
            .field("garnishment_percentage", &self.garnishment_percentage)
            .field("economy_events_enabled", &self.economy_events_enabled)
            .field(
                "llm_api_key",
                &self.llm_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("ai_model", &self.ai_model)
            .field("ai_timeout_seconds", &self.ai_timeout_seconds)
            .field("ai_max_tokens", &self.ai_max_tokens)
            .field(
                "production_ai_service",
                &self.production_ai_service.as_ref().map(|_| "configured"),
            )
            .field("dig_llm_enabled", &self.dig_llm_enabled)
            .field("pet_enabled", &self.pet_enabled)
            .field("vanity_tax_rate", &self.vanity_tax_rate)
            .field("opendota", &self.opendota)
            .finish()
    }
}

#[derive(Debug)]
pub struct PersistentVanityTaxService {
    service: VanityTaxService,
}

impl PersistentVanityTaxService {
    fn new(repository: Arc<VanityTaxRepository>, rate: f64) -> Self {
        Self {
            service: VanityTaxService::with_rate_fraction(Some(repository), rate),
        }
    }

    pub fn refresh_guild(
        &self,
        guild_id: i64,
        members: &[VanityMember],
    ) -> Result<(), VanityTaxServiceError> {
        self.service
            .refresh_guild(guild_id, members, None)
            .map(|_| ())
    }

    #[must_use]
    pub fn begin_refresh(&self, guild_id: i64) -> u64 {
        self.service.begin_refresh(guild_id)
    }

    pub fn refresh_guild_generation(
        &self,
        guild_id: i64,
        members: &[VanityMember],
        generation: u64,
    ) -> Result<bool, VanityTaxServiceError> {
        self.service
            .refresh_guild(guild_id, members, Some(generation))
    }

    pub fn update_member(&self, guild_id: i64, discord_id: i64, nickname: Option<&str>) {
        self.service.update_member(guild_id, discord_id, nickname);
    }

    pub fn remove_member(&self, guild_id: i64, discord_id: i64) {
        self.service.remove_member(guild_id, discord_id);
    }

    pub fn set_manual_taxation(
        &self,
        guild_id: i64,
        discord_id: i64,
        enforced: bool,
        actor_id: i64,
    ) -> Result<(), VanityTaxServiceError> {
        self.service
            .set_manual_taxation(guild_id, discord_id, enforced, actor_id)
    }

    #[must_use]
    pub fn calculate_tax(&self, discord_id: i64, guild_id: i64, profit: i64) -> i64 {
        self.service
            .calculate_tax(discord_id, Some(guild_id), profit)
    }

    #[must_use]
    pub fn taxable_ids(&self, guild_id: i64) -> BTreeSet<i64> {
        self.service.taxable_ids(Some(guild_id))
    }

    #[must_use]
    pub fn eligibility_status(&self, guild_id: i64, discord_id: i64) -> VanityEligibility {
        self.service.eligibility_status(Some(guild_id), discord_id)
    }

    #[must_use]
    pub fn is_manually_taxed(&self, guild_id: i64, discord_id: i64) -> bool {
        self.service.is_manually_taxed(Some(guild_id), discord_id)
    }
}

#[derive(Debug)]
pub struct ContainerComponents {
    pub vanity_tax_service: Arc<PersistentVanityTaxService>,
}

#[derive(Debug, Error)]
pub enum ContainerError {
    #[error("service container has not been initialized")]
    NotInitialized,
}

#[derive(Debug)]
pub struct ServiceContainer {
    db_path: String,
    options: ServiceContainerOptions,
    components: Option<ContainerComponents>,
}

impl ServiceContainer {
    #[must_use]
    pub fn new(db_path: impl AsRef<Path>, options: ServiceContainerOptions) -> Self {
        Self {
            db_path: db_path.as_ref().to_string_lossy().into_owned(),
            options,
            components: None,
        }
    }

    #[must_use]
    pub fn db_path(&self) -> &str {
        &self.db_path
    }

    #[must_use]
    pub const fn initialized(&self) -> bool {
        self.components.is_some()
    }

    pub fn initialize(&mut self) {
        if self.components.is_some() {
            return;
        }
        if self.db_path == ":memory:" {
            let sequence = NEXT_MEMORY_DATABASE.fetch_add(1, Ordering::Relaxed);
            self.db_path = format!(
                "file:cama-rust-container-{}-{sequence}?mode=memory&cache=shared",
                std::process::id()
            );
        }
        let vanity_repository = Arc::new(VanityTaxRepository::new(&self.db_path));
        self.components = Some(ContainerComponents {
            vanity_tax_service: Arc::new(PersistentVanityTaxService::new(
                vanity_repository,
                self.options.vanity_tax_rate,
            )),
        });
    }

    pub fn components(&self) -> Result<&ContainerComponents, ContainerError> {
        self.components
            .as_ref()
            .ok_or(ContainerError::NotInitialized)
    }
}

#[cfg(test)]
#[path = "service_container/tests.rs"]
mod tests;
