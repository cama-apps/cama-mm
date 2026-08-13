//! Typed composition root for the Python-to-Rust lift-and-shift.
//!
//! This module ports the dependency graph, stable component identity, optional
//! AI switches, bot exposure allowlist, persisted vanity-tax cache, and the
//! persisted economy-edict gate. Discord objects and provider transports remain
//! adapter responsibilities until the production runtime composes them; Rust
//! schema initialization is likewise a mandatory cutover dependency.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cama_db::economy_event_repository::{
    EconomyEventRepository, EconomyEventRepositoryError, EventDirection, PolicyMode, PolicyState,
};
use cama_db::vanity_tax_repository::VanityTaxRepository;
use chrono::{DateTime, Days, Utc};
use thiserror::Error;

use crate::ai_services::AIService;
use crate::disburse_service::{EconomyEdict, ProposalBlock, ProposalEligibility};
use crate::opendota_http::OpenDotaRuntimeServices;
pub use crate::vanity_tax_service::VanityMember;
use crate::vanity_tax_service::{VanityEligibility, VanityTaxService, VanityTaxServiceError};

pub const OPTIONAL_API_KEY_FIELDS: &[&str] = &["llm_api_key"];
pub const REMOVED_CONSTRUCTOR_FIELDS: &[&str] = &[
    "loan_cooldown_seconds",
    "loan_max_amount",
    "loan_fee_rate",
    "cerebras_api_key",
    "economy_recovery_mode",
];

/// The deliberately narrow surface published to Discord adapters.
pub const BOT_EXPOSURE_NAMES: &[&str] = &[
    "player_repo",
    "match_repo",
    "bankruptcy_repo",
    "low_priority_repo",
    "player_service",
    "match_service",
    "betting_service",
    "loan_service",
    "bankruptcy_service",
    "vanity_tax_service",
    "prediction_service",
    "lobby_service",
    "lobby_manager",
    "gambling_stats_service",
    "balance_history_service",
    "guild_config_service",
    "recalibration_service",
    "moderation_service",
    "disburse_service",
    "tax_service",
    "economy_event_service",
    "match_enrichment_service",
    "match_discovery_service",
    "pairings_service",
    "soft_avoid_service",
    "survey_service",
    "package_deal_service",
    "tip_service",
    "opendota_player_service",
    "rating_comparison_service",
    "neon_degen_service",
    "wrapped_service",
    "mana_service",
    "mana_repo",
    "mana_effects_service",
    "protection_service",
    "buff_service",
    "dig_service",
    "dig_flavor_service",
    "duel_service",
    "duel_flavor_service",
    "reminder_service",
    "mafia_service",
    "mafia_flavor_service",
    "pet_service",
    "pet_evolution_service",
    "pet_flavor_service",
    "pet_brawl_service",
    "player_trivia_service",
    "sql_query_service",
    "flavor_text_service",
];

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
            vanity_tax_rate: 0.10,
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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ComponentId(u64);

#[derive(Debug)]
pub struct ComponentNode {
    id: ComponentId,
    name: &'static str,
}

impl ComponentNode {
    #[must_use]
    pub const fn id(&self) -> ComponentId {
        self.id
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

#[derive(Debug, Default)]
struct MemoryCatalog {
    players: BTreeMap<i64, BTreeSet<i64>>,
}

#[derive(Debug)]
pub struct DatabaseRuntimeRef {
    component: Arc<ComponentNode>,
    db_path: String,
    memory: Option<Arc<Mutex<MemoryCatalog>>>,
}

impl DatabaseRuntimeRef {
    #[must_use]
    pub fn db_path(&self) -> &str {
        &self.db_path
    }

    #[must_use]
    pub const fn component(&self) -> &Arc<ComponentNode> {
        &self.component
    }

    #[must_use]
    pub const fn is_normalized_shared_memory(&self) -> bool {
        self.memory.is_some()
    }
}

#[derive(Debug)]
pub struct RepositoryRef {
    component: Arc<ComponentNode>,
    db_path: String,
    memory: Option<Arc<Mutex<MemoryCatalog>>>,
}

impl RepositoryRef {
    #[must_use]
    pub const fn component(&self) -> &Arc<ComponentNode> {
        &self.component
    }

    #[must_use]
    pub fn db_path(&self) -> &str {
        &self.db_path
    }

    /// In-memory player snapshot used by the composition boundary before a
    /// concrete SQLite runtime adapter is attached.
    #[must_use]
    pub fn player_ids(&self, guild_id: i64) -> Option<Vec<i64>> {
        self.memory.as_ref().map(|memory| {
            memory
                .lock()
                .expect("container memory catalog mutex poisoned")
                .players
                .get(&guild_id)
                .map_or_else(Vec::new, |players| players.iter().copied().collect())
        })
    }
}

#[derive(Debug)]
pub struct RepositoryServiceRef {
    component: Arc<ComponentNode>,
    repository: Arc<RepositoryRef>,
}

impl RepositoryServiceRef {
    #[must_use]
    pub const fn component(&self) -> &Arc<ComponentNode> {
        &self.component
    }

    #[must_use]
    pub const fn repository(&self) -> &Arc<RepositoryRef> {
        &self.repository
    }
}

pub struct AiServiceRef {
    component: Arc<ComponentNode>,
    api_key: String,
    model: String,
    timeout_seconds: f64,
    max_tokens: i64,
    production: Option<Arc<AIService>>,
    request_repository: Arc<RepositoryRef>,
}

impl fmt::Debug for AiServiceRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AiServiceRef")
            .field("component", &self.component)
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .field("timeout_seconds", &self.timeout_seconds)
            .field("max_tokens", &self.max_tokens)
            .field(
                "production",
                &self.production.as_ref().map(|_| "configured"),
            )
            .field("request_repository", &self.request_repository)
            .finish()
    }
}

impl AiServiceRef {
    #[must_use]
    pub const fn component(&self) -> &Arc<ComponentNode> {
        &self.component
    }

    #[must_use]
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    #[must_use]
    pub const fn timeout_seconds(&self) -> f64 {
        self.timeout_seconds
    }

    #[must_use]
    pub const fn max_tokens(&self) -> i64 {
        self.max_tokens
    }

    #[must_use]
    pub const fn production(&self) -> Option<&Arc<AIService>> {
        self.production.as_ref()
    }

    #[must_use]
    pub const fn request_repository(&self) -> &Arc<RepositoryRef> {
        &self.request_repository
    }
}

#[derive(Debug)]
pub struct DuelFlavorServiceRef {
    component: Arc<ComponentNode>,
    ai_service: Option<Arc<AiServiceRef>>,
    guild_config_repository: Arc<RepositoryRef>,
}

impl DuelFlavorServiceRef {
    #[must_use]
    pub const fn component(&self) -> &Arc<ComponentNode> {
        &self.component
    }

    #[must_use]
    pub const fn ai_service(&self) -> Option<&Arc<AiServiceRef>> {
        self.ai_service.as_ref()
    }

    #[must_use]
    pub const fn guild_config_repository(&self) -> &Arc<RepositoryRef> {
        &self.guild_config_repository
    }
}

#[derive(Debug)]
pub struct BettingServiceRef {
    component: Arc<ComponentNode>,
    garnishment_service: Arc<ComponentNode>,
    bankruptcy_service: Arc<ComponentNode>,
}

impl BettingServiceRef {
    #[must_use]
    pub const fn component(&self) -> &Arc<ComponentNode> {
        &self.component
    }

    #[must_use]
    pub const fn garnishment_service(&self) -> &Arc<ComponentNode> {
        &self.garnishment_service
    }

    #[must_use]
    pub const fn bankruptcy_service(&self) -> &Arc<ComponentNode> {
        &self.bankruptcy_service
    }
}

#[derive(Debug)]
pub struct MatchServiceRef {
    component: Arc<ComponentNode>,
    betting_service: Arc<ComponentNode>,
    state_service: Arc<ComponentNode>,
}

impl MatchServiceRef {
    #[must_use]
    pub const fn component(&self) -> &Arc<ComponentNode> {
        &self.component
    }

    #[must_use]
    pub const fn betting_service(&self) -> &Arc<ComponentNode> {
        &self.betting_service
    }

    #[must_use]
    pub const fn state_service(&self) -> &Arc<ComponentNode> {
        &self.state_service
    }
}

#[derive(Debug)]
pub struct LobbyServiceRef {
    component: Arc<ComponentNode>,
    match_state_service: Arc<ComponentNode>,
}

impl LobbyServiceRef {
    #[must_use]
    pub const fn component(&self) -> &Arc<ComponentNode> {
        &self.component
    }

    #[must_use]
    pub const fn match_state_service(&self) -> &Arc<ComponentNode> {
        &self.match_state_service
    }
}

#[derive(Debug)]
pub struct DigServiceRef {
    component: Arc<ComponentNode>,
    pet_service: Option<Arc<ComponentNode>>,
}

impl DigServiceRef {
    #[must_use]
    pub const fn component(&self) -> &Arc<ComponentNode> {
        &self.component
    }

    #[must_use]
    pub const fn pet_service(&self) -> Option<&Arc<ComponentNode>> {
        self.pet_service.as_ref()
    }
}

#[derive(Debug)]
pub struct NeonDegenServiceRef {
    component: Arc<ComponentNode>,
    dig_llm_enabled: bool,
}

impl NeonDegenServiceRef {
    #[must_use]
    pub const fn component(&self) -> &Arc<ComponentNode> {
        &self.component
    }

    #[must_use]
    pub const fn dig_llm_enabled(&self) -> bool {
        self.dig_llm_enabled
    }
}

#[derive(Debug)]
pub struct PersistentVanityTaxService {
    component: Arc<ComponentNode>,
    service: VanityTaxService,
}

impl PersistentVanityTaxService {
    fn new(component: Arc<ComponentNode>, repository: Arc<VanityTaxRepository>, rate: f64) -> Self {
        Self {
            component,
            service: VanityTaxService::with_rate_fraction(Some(repository), rate),
        }
    }

    #[must_use]
    pub const fn component(&self) -> &Arc<ComponentNode> {
        &self.component
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
pub struct ContainerEconomyEventService {
    component: Arc<ComponentNode>,
    repository: Arc<EconomyEventRepository>,
    enabled: bool,
}

impl ContainerEconomyEventService {
    fn new(
        component: Arc<ComponentNode>,
        repository: Arc<EconomyEventRepository>,
        enabled: bool,
    ) -> Self {
        Self {
            component,
            repository,
            enabled,
        }
    }

    #[must_use]
    pub const fn component(&self) -> &Arc<ComponentNode> {
        &self.component
    }

    pub fn ensure_policy(
        &self,
        guild_id: i64,
        now: i64,
    ) -> Result<PolicyState, EconomyEventRepositoryError> {
        self.repository
            .ensure_policy_state(Some(guild_id), PolicyMode::Normal, 0.02, 0.10, now)
    }

    pub fn reserve_voting_restriction(
        &self,
        guild_id: i64,
        now: i64,
    ) -> Result<Option<EconomyEdict>, ContainerError> {
        if !self.enabled {
            return Ok(None);
        }
        let timestamp =
            DateTime::<Utc>::from_timestamp(now, 0).ok_or(ContainerError::InvalidTimestamp(now))?;
        let today = timestamp.date_naive();
        for date in [today, today.checked_sub_days(Days::new(1)).unwrap_or(today)] {
            let event_date = date.format("%Y-%m-%d").to_string();
            let Some(event) = self
                .repository
                .get_event_for_date(Some(guild_id), &event_date)?
            else {
                continue;
            };
            if event.direction == EventDirection::Deflationary
                && event.severity >= 3
                && event.starts_at <= now
                && now < event.ends_at
            {
                return Ok(Some(EconomyEdict {
                    event_id: event.event_id,
                    name: event.name,
                    severity: u8::try_from(event.severity.clamp(1, 5)).unwrap_or(1),
                }));
            }
        }
        Ok(None)
    }
}

#[derive(Debug)]
pub struct ContainerDisburseService {
    component: Arc<ComponentNode>,
    voting_enabled: bool,
    economy_events: Arc<ContainerEconomyEventService>,
}

impl ContainerDisburseService {
    #[must_use]
    pub const fn component(&self) -> &Arc<ComponentNode> {
        &self.component
    }

    #[must_use]
    pub const fn voting_enabled(&self) -> bool {
        self.voting_enabled
    }

    pub fn can_propose(
        &self,
        guild_id: i64,
        now: i64,
    ) -> Result<ProposalEligibility, ContainerError> {
        if !self.voting_enabled {
            return Ok(ProposalEligibility::Blocked(
                ProposalBlock::MonetaryRecovery,
            ));
        }
        if self
            .economy_events
            .reserve_voting_restriction(guild_id, now)?
            .is_some()
        {
            return Ok(ProposalEligibility::Blocked(ProposalBlock::EconomyEdict));
        }
        Ok(ProposalEligibility::Allowed)
    }
}

#[derive(Debug)]
pub struct ContainerComponents {
    pub database_runtime: Arc<DatabaseRuntimeRef>,
    pub player_repository: Arc<RepositoryRef>,
    pub match_repository: Arc<RepositoryRef>,
    pub duel_repository: Arc<RepositoryRef>,
    pub guild_config_repository: Arc<RepositoryRef>,
    pub llm_request_repository: Arc<RepositoryRef>,
    pub survey_repository: Arc<RepositoryRef>,
    pub player_service: Arc<RepositoryServiceRef>,
    pub duel_service: Arc<RepositoryServiceRef>,
    pub survey_service: Arc<RepositoryServiceRef>,
    pub betting_service: Arc<BettingServiceRef>,
    pub match_service: Arc<MatchServiceRef>,
    pub lobby_service: Arc<LobbyServiceRef>,
    pub dig_service: Arc<DigServiceRef>,
    pub neon_degen_service: Arc<NeonDegenServiceRef>,
    pub ai_service: Option<Arc<AiServiceRef>>,
    pub duel_flavor_service: Arc<DuelFlavorServiceRef>,
    pub vanity_tax_service: Arc<PersistentVanityTaxService>,
    pub economy_event_service: Arc<ContainerEconomyEventService>,
    pub disburse_service: Arc<ContainerDisburseService>,
    pub opendota: Option<Arc<OpenDotaRuntimeServices>>,
    registry: BTreeMap<&'static str, Option<Arc<ComponentNode>>>,
}

impl ContainerComponents {
    #[must_use]
    pub fn component(&self, name: &str) -> Option<&Arc<ComponentNode>> {
        self.registry.get(name).and_then(Option::as_ref)
    }

    #[must_use]
    pub fn component_slot(&self, name: &str) -> Option<&Option<Arc<ComponentNode>>> {
        self.registry.get(name)
    }
}

#[derive(Debug)]
pub struct BotExposure {
    attributes: BTreeMap<&'static str, Option<Arc<ComponentNode>>>,
}

impl BotExposure {
    #[must_use]
    pub const fn attributes(&self) -> &BTreeMap<&'static str, Option<Arc<ComponentNode>>> {
        &self.attributes
    }
}

#[derive(Debug, Error)]
pub enum ContainerError {
    #[error("service container has not been initialized")]
    NotInitialized,
    #[error("timestamp is outside chrono's supported range: {0}")]
    InvalidTimestamp(i64),
    #[error(transparent)]
    Economy(#[from] EconomyEventRepositoryError),
}

#[derive(Debug)]
pub struct ServiceContainer {
    db_path: String,
    options: ServiceContainerOptions,
    initialized: bool,
    components: Option<ContainerComponents>,
}

impl ServiceContainer {
    #[must_use]
    pub fn new(db_path: impl AsRef<Path>, options: ServiceContainerOptions) -> Self {
        Self {
            db_path: db_path.as_ref().to_string_lossy().into_owned(),
            options,
            initialized: false,
            components: None,
        }
    }

    #[must_use]
    pub fn db_path(&self) -> &str {
        &self.db_path
    }

    #[must_use]
    pub const fn initialized(&self) -> bool {
        self.initialized
    }

    pub fn initialize(&mut self) {
        if self.initialized {
            return;
        }
        let mut ids = IdAllocator::default();
        let memory =
            (self.db_path == ":memory:").then(|| Arc::new(Mutex::new(MemoryCatalog::default())));
        if memory.is_some() {
            let sequence = NEXT_MEMORY_DATABASE.fetch_add(1, Ordering::Relaxed);
            self.db_path = format!(
                "file:cama-rust-container-{}-{sequence}?mode=memory&cache=shared",
                std::process::id()
            );
        }
        let database_runtime = Arc::new(DatabaseRuntimeRef {
            component: ids.node("database_runtime"),
            db_path: self.db_path.clone(),
            memory: memory.clone(),
        });
        let player_repository = repository(&mut ids, "player_repo", &self.db_path, &memory);
        let match_repository = repository(&mut ids, "match_repo", &self.db_path, &memory);
        let duel_repository = repository(&mut ids, "duel_repo", &self.db_path, &memory);
        let guild_config_repository =
            repository(&mut ids, "guild_config_repo", &self.db_path, &memory);
        let llm_request_repository =
            repository(&mut ids, "llm_request_repo", &self.db_path, &memory);
        let survey_repository = repository(&mut ids, "survey_repo", &self.db_path, &memory);

        let player_service = Arc::new(RepositoryServiceRef {
            component: ids.node("player_service"),
            repository: Arc::clone(&player_repository),
        });
        let duel_service = Arc::new(RepositoryServiceRef {
            component: ids.node("duel_service"),
            repository: Arc::clone(&duel_repository),
        });
        let survey_service = Arc::new(RepositoryServiceRef {
            component: ids.node("survey_service"),
            repository: Arc::clone(&survey_repository),
        });
        let garnishment = ids.node("garnishment_service");
        let bankruptcy = ids.node("bankruptcy_service");
        let betting_service = Arc::new(BettingServiceRef {
            component: ids.node("betting_service"),
            garnishment_service: Arc::clone(&garnishment),
            bankruptcy_service: Arc::clone(&bankruptcy),
        });
        let match_state = ids.node("match_state_service");
        let match_service = Arc::new(MatchServiceRef {
            component: ids.node("match_service"),
            betting_service: Arc::clone(betting_service.component()),
            state_service: Arc::clone(&match_state),
        });
        let lobby_service = Arc::new(LobbyServiceRef {
            component: ids.node("lobby_service"),
            match_state_service: Arc::clone(&match_state),
        });
        let pet_node = self.options.pet_enabled.then(|| ids.node("pet_service"));
        let dig_service = Arc::new(DigServiceRef {
            component: ids.node("dig_service"),
            pet_service: pet_node.clone(),
        });
        let neon_degen_service = Arc::new(NeonDegenServiceRef {
            component: ids.node("neon_degen_service"),
            dig_llm_enabled: self.options.dig_llm_enabled,
        });
        let ai_service = self.options.llm_api_key.as_ref().map(|api_key| {
            Arc::new(AiServiceRef {
                component: ids.node("ai_service"),
                api_key: api_key.clone(),
                model: self.options.ai_model.clone(),
                timeout_seconds: self.options.ai_timeout_seconds,
                max_tokens: self.options.ai_max_tokens,
                production: self.options.production_ai_service.clone(),
                request_repository: Arc::clone(&llm_request_repository),
            })
        });
        let duel_flavor_service = Arc::new(DuelFlavorServiceRef {
            component: ids.node("duel_flavor_service"),
            ai_service: ai_service.clone(),
            guild_config_repository: Arc::clone(&guild_config_repository),
        });

        let vanity_component = ids.node("vanity_tax_service");
        let vanity_repository = Arc::new(VanityTaxRepository::new(&self.db_path));
        let vanity_tax_service = Arc::new(PersistentVanityTaxService::new(
            Arc::clone(&vanity_component),
            vanity_repository,
            self.options.vanity_tax_rate,
        ));
        let economy_component = ids.node("economy_event_service");
        let economy_repository = Arc::new(EconomyEventRepository::new(&self.db_path));
        let economy_event_service = Arc::new(ContainerEconomyEventService::new(
            Arc::clone(&economy_component),
            economy_repository,
            self.options.economy_events_enabled,
        ));
        let disburse_service = Arc::new(ContainerDisburseService {
            component: ids.node("disburse_service"),
            voting_enabled: true,
            economy_events: Arc::clone(&economy_event_service),
        });

        let mut registry = BTreeMap::new();
        register(&mut registry, database_runtime.component());
        for repository in [
            &player_repository,
            &match_repository,
            &duel_repository,
            &guild_config_repository,
            &llm_request_repository,
            &survey_repository,
        ] {
            register(&mut registry, repository.component());
        }
        for component in [
            player_service.component(),
            duel_service.component(),
            survey_service.component(),
            betting_service.component(),
            match_service.component(),
            lobby_service.component(),
            dig_service.component(),
            neon_degen_service.component(),
            duel_flavor_service.component(),
            vanity_tax_service.component(),
            economy_event_service.component(),
            disburse_service.component(),
            &garnishment,
            &bankruptcy,
            &match_state,
        ] {
            register(&mut registry, component);
        }
        if let Some(ai) = &ai_service {
            register(&mut registry, ai.component());
        }

        for name in BOT_EXPOSURE_NAMES {
            if registry.contains_key(name) {
                continue;
            }
            let present = match *name {
                "dig_flavor_service" => ai_service.is_some() && self.options.dig_llm_enabled,
                "sql_query_service" | "flavor_text_service" => ai_service.is_some(),
                "pet_service"
                | "pet_evolution_service"
                | "pet_flavor_service"
                | "pet_brawl_service" => self.options.pet_enabled,
                _ => true,
            };
            registry.insert(name, present.then(|| ids.node(name)));
        }
        if let Some(pet) = pet_node {
            registry.insert("pet_service", Some(pet));
        }
        let components = ContainerComponents {
            database_runtime,
            player_repository,
            match_repository,
            duel_repository,
            guild_config_repository,
            llm_request_repository,
            survey_repository,
            player_service,
            duel_service,
            survey_service,
            betting_service,
            match_service,
            lobby_service,
            dig_service,
            neon_degen_service,
            ai_service,
            duel_flavor_service,
            vanity_tax_service,
            economy_event_service,
            disburse_service,
            opendota: self.options.opendota.clone(),
            registry,
        };
        self.components = Some(components);
        self.initialized = true;
    }

    pub fn components(&self) -> Result<&ContainerComponents, ContainerError> {
        self.components
            .as_ref()
            .ok_or(ContainerError::NotInitialized)
    }

    pub fn expose_to_bot(&self) -> Result<BotExposure, ContainerError> {
        let components = self.components()?;
        let attributes = BOT_EXPOSURE_NAMES
            .iter()
            .map(|name| {
                (
                    *name,
                    components.component_slot(name).cloned().unwrap_or(None),
                )
            })
            .collect();
        Ok(BotExposure { attributes })
    }
}

#[derive(Default)]
struct IdAllocator {
    next: u64,
}

impl IdAllocator {
    fn node(&mut self, name: &'static str) -> Arc<ComponentNode> {
        self.next += 1;
        Arc::new(ComponentNode {
            id: ComponentId(self.next),
            name,
        })
    }
}

fn repository(
    ids: &mut IdAllocator,
    name: &'static str,
    db_path: &str,
    memory: &Option<Arc<Mutex<MemoryCatalog>>>,
) -> Arc<RepositoryRef> {
    Arc::new(RepositoryRef {
        component: ids.node(name),
        db_path: db_path.to_owned(),
        memory: memory.clone(),
    })
}

fn register(
    registry: &mut BTreeMap<&'static str, Option<Arc<ComponentNode>>>,
    component: &Arc<ComponentNode>,
) {
    registry.insert(component.name(), Some(Arc::clone(component)));
}

#[cfg(test)]
#[path = "service_container/tests.rs"]
mod tests;
