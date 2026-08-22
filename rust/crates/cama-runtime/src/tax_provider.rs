//! Production Discord provider for the complete Python `/tax` surface.
//!
//! Every SQLite call is offloaded from Tokio. The central fine mutation is a
//! single `BEGIN IMMEDIATE` operation in `cama-db`; the two ephemeral pagers
//! keep requester ownership and their five-minute lifetime in process, exactly
//! like discord.py views.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cama_app::economy_event_service::{
    DeterministicEventRng, EconomyEventConfig, EconomyEventService, FixedClock, MemoryEconomy,
    MemoryEventStore,
};
use cama_app::service_container::PersistentVanityTaxService;
use cama_app::tax_commands::{
    BalanceSheet, BankruptcyAction, BankruptcyCommandRequest, BankruptcyRequest, BankruptcyResult,
    CommandResponse, Distribution, EconomyDirection, EconomyEffects, EconomyEvent, EconomyPolicy,
    EconomyStatusPort, FineCommandRequest, FineRequest, FineResult, FlowIndicator, LedgerAccount,
    LedgerCommandRequest, LedgerReadPort, LedgerRow, ManualTaxationRequest, MonetaryTrend,
    PLAYER_LEDGER_DEFAULT_LIMIT, PlayerAuditReadPort, PlayerSnapshot, PolicyStatus,
    PredictionExposure, PredictionPosition, PredictionSummary, ResetCooldownCommandRequest,
    ResetFineCooldownRequest, ResetFineCooldownResult, SpellIconPort, TaxCommandAuthorization,
    TaxEmbed, TaxEnforcementPort, TaxLedgerView, TaxPlayerLedgerView, UserRef, VanityEligibility,
    VanityTaxPort, bankruptcy_command, build_player_embed, clamp_ledger_page, event_command,
    fine_command, ledger_command, ledger_offset, ledger_total_pages, policy_command,
    resetcooldown_command, vanity_command,
};
use cama_app::vanity_tax_service::VanityEligibility as PersistentVanityEligibility;
use cama_db::economy_event_repository::{EconomyEventRepository, EventDirection, PolicyMode};
use cama_db::tax_repository::{
    TaxBankruptcyAction, TaxBankruptcyOutcome, TaxBankruptcyRequest, TaxFineOutcome,
    TaxFineRequest, TaxGuildSnapshot, TaxLedgerEntry, TaxLedgerSourceTotal, TaxRepository,
    TaxRepositoryError,
};
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::permissions::{
    DiscordPermissions, PermissionContext, has_admin_permission, has_tax_man_permission,
};
use chrono::Utc;

use crate::application_config::ApplicationConfig;
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionActionRow, InteractionButton, InteractionButtonStyle, InteractionEmbed,
    InteractionHandler, InteractionHandlerError, InteractionOption, InteractionRequest,
    InteractionResponder, InteractionResponse, InteractionValue, RegistrationError,
    RegistrationProvider, RegistryBuilder,
};

const GUILD_ONLY_MESSAGE: &str = "This command can only be used in a server.";
const COMPONENT_PREFIX: &str = "tax:";
const LEDGER_TIMEOUT_SECONDS: i64 = 300;
const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;

#[derive(Clone)]
pub struct TaxRegistrationProvider {
    handler: Arc<TaxHandler>,
}

impl TaxRegistrationProvider {
    /// Compose the live provider over the already admitted SQLite database.
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        vanity_tax: Arc<PersistentVanityTaxService>,
    ) -> Self {
        let values = &config.values;
        let tax = Arc::new(SqliteTaxPort {
            repository: TaxRepository::new(
                database_path.as_ref(),
                values.prediction_contract_value,
                values.prediction_initial_fair_default,
            ),
            fine_cooldown_seconds: values.tax_fine_cooldown_seconds,
        });
        let economy_config = EconomyEventConfig {
            enabled: values.economy_events_enabled,
            normal_annual_rate: values.economy_normal_annual_rate,
            inflation_ceiling: values.economy_inflation_ceiling,
            lookback_days: values.economy_event_lookback_days,
            max_reserve_burn_pct: values.economy_event_max_reserve_burn_pct,
            max_wallet_burn_pct: values.economy_event_max_wallet_burn_pct,
            trigger_hour_local: u8::try_from(values.economy_event_trigger_hour_local.clamp(0, 23))
                .unwrap_or(10),
        }
        .normalized();
        let economy = Arc::new(SqliteEconomyStatusPort {
            repository: EconomyEventRepository::new(database_path),
            config: economy_config,
        });
        let admin_user_ids = config
            .identities
            .admin_user_ids
            .iter()
            .filter_map(|id| u64::try_from(*id).ok())
            .collect();
        let tax_man_user_ids = config
            .identities
            .tax_man_user_ids
            .iter()
            .filter_map(|id| u64::try_from(*id).ok())
            .collect();
        Self {
            handler: Arc::new(TaxHandler {
                tax,
                economy,
                vanity: Arc::new(PersistentVanityPort {
                    service: vanity_tax,
                    rate_fraction: values.vanity_tax_rate,
                }),
                admin_user_ids,
                tax_man_user_ids,
                bankruptcy_penalty_games: values.bankruptcy_penalty_games,
                vanity_tax_rate: values.vanity_tax_rate,
                views: Arc::new(Mutex::new(BTreeMap::new())),
            }),
        }
    }
}

impl RegistrationProvider for TaxRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "tax".to_owned(),
            description: "Jopacoin economy and Tax Man tools".to_owned(),
            options: tax_subcommands(self.handler.vanity_tax_rate),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

fn tax_subcommands(vanity_tax_rate: f64) -> Vec<CommandOptionSpec> {
    let subcommand = |name: &str, description: &str, options: Vec<CommandOptionSpec>| {
        CommandOptionSpec::new(name, description, CommandOptionKind::Subcommand).options(options)
    };
    let mut bankruptcy_action = CommandOptionSpec::new(
        "action",
        "Add or remove the bankruptcy modifier",
        CommandOptionKind::String,
    )
    .required(true);
    bankruptcy_action.choices = vec![
        CommandOptionChoice::String {
            name: "add".to_owned(),
            value: "add".to_owned(),
        },
        CommandOptionChoice::String {
            name: "remove".to_owned(),
            value: "remove".to_owned(),
        },
    ];
    let mut bankruptcy_games = CommandOptionSpec::new(
        "games",
        "Penalty games to add; ignored when removing",
        CommandOptionKind::Integer,
    );
    bankruptcy_games.min_integer = Some(0);
    bankruptcy_games.max_integer = Some(100);
    let mut ledger_limit = CommandOptionSpec::new(
        "limit",
        "Number of ledger entries to show",
        CommandOptionKind::Integer,
    );
    ledger_limit.min_integer = Some(1);
    ledger_limit.max_integer = Some(25);
    let mut ledger_page =
        CommandOptionSpec::new("page", "Ledger page to show", CommandOptionKind::Integer);
    ledger_page.min_integer = Some(1);
    ledger_page.max_integer = Some(10_000);
    vec![
        subcommand("audit", "View guild-wide monetary exposure", Vec::new()),
        subcommand("policy", "View daily Jopacoin monetary policy", Vec::new()),
        subcommand(
            "event",
            "Reveal today's server-wide Jopacoin event",
            Vec::new(),
        ),
        subcommand(
            "vanity",
            &format!(
                "Check the nickname vanity tax ({:.0}% of profits)",
                vanity_tax_rate * 100.0
            ),
            vec![
                CommandOptionSpec::new(
                    "user",
                    "Player to check (defaults to you)",
                    CommandOptionKind::User,
                ),
                CommandOptionSpec::new(
                    "taxable",
                    "ADMIN_USER_IDS only: force tax or restore the nickname rule",
                    CommandOptionKind::Boolean,
                ),
            ],
        ),
        subcommand(
            "player",
            "View one player's full monetary exposure",
            vec![
                CommandOptionSpec::new("user", "Player to audit", CommandOptionKind::User)
                    .required(true),
            ],
        ),
        subcommand(
            "fine",
            "Levy a Tax Man fine against a player",
            vec![
                CommandOptionSpec::new("user", "Player to fine", CommandOptionKind::User)
                    .required(true),
                CommandOptionSpec::new(
                    "amount",
                    "Jopacoin amount to fine",
                    CommandOptionKind::Integer,
                )
                .required(true),
                CommandOptionSpec::new(
                    "reason",
                    "Reason recorded in the central ledger",
                    CommandOptionKind::String,
                ),
            ],
        ),
        subcommand(
            "resetcooldown",
            "Reset a player's Tax Man fine cooldown",
            vec![
                CommandOptionSpec::new(
                    "user",
                    "Player whose Tax Man fine cooldown should reset",
                    CommandOptionKind::User,
                )
                .required(true),
            ],
        ),
        subcommand(
            "bankruptcy",
            "Add or remove a player's bankruptcy modifier",
            vec![
                CommandOptionSpec::new("user", "Player to modify", CommandOptionKind::User)
                    .required(true),
                bankruptcy_action,
                bankruptcy_games,
                CommandOptionSpec::new(
                    "reason",
                    "Reason for the modifier change",
                    CommandOptionKind::String,
                ),
            ],
        ),
        subcommand(
            "ledger",
            "View recent central ledger entries",
            vec![
                CommandOptionSpec::new("user", "Limit to one player", CommandOptionKind::User),
                ledger_limit,
                ledger_page,
            ],
        ),
    ]
}

struct TaxHandler {
    tax: Arc<SqliteTaxPort>,
    economy: Arc<SqliteEconomyStatusPort>,
    vanity: Arc<PersistentVanityPort>,
    admin_user_ids: Vec<u64>,
    tax_man_user_ids: Vec<u64>,
    bankruptcy_penalty_games: i64,
    vanity_tax_rate: f64,
    views: Arc<Mutex<BTreeMap<u64, StoredTaxView>>>,
}

#[derive(Clone)]
enum StoredTaxView {
    Ledger {
        expires_at: Instant,
        view: TaxLedgerView,
    },
    Player {
        expires_at: Instant,
        view: TaxPlayerLedgerView,
    },
}

impl StoredTaxView {
    fn ledger(view: TaxLedgerView) -> Self {
        Self::ledger_at(view, view_expiration())
    }

    fn player(view: TaxPlayerLedgerView) -> Self {
        Self::player_at(view, view_expiration())
    }

    const fn ledger_at(view: TaxLedgerView, expires_at: Instant) -> Self {
        Self::Ledger { expires_at, view }
    }

    const fn player_at(view: TaxPlayerLedgerView, expires_at: Instant) -> Self {
        Self::Player { expires_at, view }
    }

    fn refreshed(self) -> Self {
        let expires_at = view_expiration();
        match self {
            Self::Ledger { view, .. } => Self::ledger_at(view, expires_at),
            Self::Player { view, .. } => Self::player_at(view, expires_at),
        }
    }

    const fn expires_at(&self) -> Instant {
        match self {
            Self::Ledger { expires_at, .. } | Self::Player { expires_at, .. } => *expires_at,
        }
    }
}

fn view_expiration() -> Instant {
    Instant::now() + Duration::from_secs(LEDGER_TIMEOUT_SECONDS as u64)
}

#[derive(Clone)]
struct TaxCommandContext {
    interaction_id: u64,
    actor_id: u64,
    actor_display_name: String,
    guild_id: u64,
    member_permissions: Option<u64>,
    subcommand: String,
    options: Vec<InteractionOption>,
}

#[async_trait]
impl InteractionHandler for TaxHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command {
                interaction_id,
                name,
                user_id,
                user_display_name,
                guild_id,
                member_permissions,
                options,
                ..
            } => {
                if name != "tax" {
                    return Err(format!("tax handler received command {name:?}").into());
                }
                let Some(guild_id) = guild_id else {
                    return respond_ephemeral(&responder, GUILD_ONLY_MESSAGE).await;
                };
                let (subcommand, options) = nested_subcommand(&options)?;
                self.handle_command(
                    TaxCommandContext {
                        interaction_id,
                        actor_id: user_id,
                        actor_display_name: user_display_name,
                        guild_id,
                        member_permissions,
                        subcommand,
                        options,
                    },
                    responder,
                )
                .await
            }
            InteractionRequest::Component {
                custom_id, user_id, ..
            } => self.handle_component(&custom_id, user_id, responder).await,
            _ => Err("tax provider accepts slash commands and tax pager buttons only".into()),
        }
    }
}

impl TaxHandler {
    async fn handle_command(
        &self,
        command: TaxCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match command.subcommand.as_str() {
            "audit" => self.audit(command, responder).await,
            "policy" => self.policy(command, responder).await,
            "event" => self.event(command, responder).await,
            "vanity" => self.vanity(command, responder).await,
            "player" => self.player(command, responder).await,
            "fine" => self.fine(command, responder).await,
            "resetcooldown" => self.resetcooldown(command, responder).await,
            "bankruptcy" => self.bankruptcy(command, responder).await,
            "ledger" => self.ledger(command, responder).await,
            other => Err(format!("unknown /tax subcommand {other:?}").into()),
        }
    }

    fn permission_context(&self, command: &TaxCommandContext) -> PermissionContext {
        let context = PermissionContext::new(command.actor_id);
        command.member_permissions.map_or(context, |permissions| {
            context.with_guild_member_permissions(DiscordPermissions {
                administrator: permissions & ADMINISTRATOR_PERMISSION != 0,
                manage_guild: permissions & MANAGE_GUILD_PERMISSION != 0,
            })
        })
    }

    async fn require_tax_man(
        &self,
        command: &TaxCommandContext,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<bool, InteractionHandlerError> {
        if has_tax_man_permission(
            &self.permission_context(command),
            &self.admin_user_ids,
            &self.tax_man_user_ids,
        ) {
            Ok(true)
        } else {
            respond_ephemeral(responder, "Only Tax Men can use this command.").await?;
            Ok(false)
        }
    }

    fn authorization(&self, command: &TaxCommandContext) -> TaxCommandAuthorization<'_> {
        TaxCommandAuthorization {
            permissions: self.permission_context(command),
            admin_user_ids: &self.admin_user_ids,
            tax_man_user_ids: &self.tax_man_user_ids,
        }
    }

    async fn audit(
        &self,
        command: TaxCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_tax_man(&command, &responder).await? {
            return Ok(());
        }
        defer(&responder, true).await?;
        let guild_id = signed_id(command.guild_id, "guild")?;
        let snapshot_tax = Arc::clone(&self.tax);
        let sources_tax = Arc::clone(&self.tax);
        let (snapshot, sources) = tokio::join!(
            tokio::task::spawn_blocking(move || snapshot_tax.guild_snapshot(guild_id)),
            tokio::task::spawn_blocking(move || sources_tax.source_totals(guild_id, 8)),
        );
        let result = match (snapshot, sources) {
            (Ok(Ok(snapshot)), Ok(Ok(sources))) => Ok((snapshot, sources)),
            (Err(error), _) | (_, Err(error)) => {
                return Err(format!("tax audit task failed: {error}").into());
            }
            _ => Err(()),
        };
        match result {
            Ok((snapshot, sources)) => {
                followup(
                    &responder,
                    InteractionResponse::message("")
                        .embed(build_audit_embed(&snapshot, &sources))
                        .ephemeral(),
                )
                .await
            }
            Err(()) => {
                followup_ephemeral(&responder, "Couldn't load the Tax Man audit right now.").await
            }
        }
    }

    async fn policy(
        &self,
        command: TaxCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_tax_man(&command, &responder).await? {
            return Ok(());
        }
        defer(&responder, true).await?;
        let economy = Arc::clone(&self.economy);
        let guild_id = command.guild_id;
        let authorization = self.authorization(&command);
        let permissions = authorization.permissions;
        let admin = self.admin_user_ids.clone();
        let tax_men = self.tax_man_user_ids.clone();
        let response = tokio::task::spawn_blocking(move || {
            let authorization = TaxCommandAuthorization {
                permissions,
                admin_user_ids: &admin,
                tax_man_user_ids: &tax_men,
            };
            policy_command(guild_id, &authorization, Some(economy.as_ref()))
        })
        .await
        .map_err(|error| format!("tax policy task failed: {error}"))?;
        followup(&responder, command_response(response)).await
    }

    async fn event(
        &self,
        command: TaxCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        defer(&responder, false).await?;
        let economy = Arc::clone(&self.economy);
        let guild_id = command.guild_id;
        let response = tokio::task::spawn_blocking(move || {
            event_command(guild_id, Some(economy.as_ref()), &CatalogSpellIcons)
        })
        .await
        .map_err(|error| format!("tax event task failed: {error}"))?;
        followup(&responder, command_response(response)).await
    }

    async fn vanity(
        &self,
        command: TaxCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let target = user_option(&command.options, "user")?.unwrap_or_else(|| UserRef {
            id: command.actor_id,
            name: command.actor_display_name.to_lowercase(),
            display_name: command.actor_display_name.clone(),
        });
        let taxable = boolean_option(&command.options, "taxable");
        let permissions = self.permission_context(&command);
        if taxable.is_some() && !has_admin_permission(&permissions, &self.admin_user_ids) {
            return respond_ephemeral(&responder, "Only admins can change vanity-tax enforcement.")
                .await;
        }
        if taxable.is_some() {
            defer(&responder, true).await?;
        }
        let vanity = Arc::clone(&self.vanity);
        let admin = self.admin_user_ids.clone();
        let guild_id = command.guild_id;
        let actor_id = command.actor_id;
        let response = tokio::task::spawn_blocking(move || {
            vanity_command(
                Some(vanity.as_ref()),
                &permissions,
                &admin,
                guild_id,
                actor_id,
                &target,
                taxable,
            )
        })
        .await
        .map_err(|error| format!("tax vanity task failed: {error}"))?;
        if taxable.is_some() {
            followup(&responder, command_response(response)).await
        } else {
            respond(&responder, command_response(response)).await
        }
    }

    async fn player(
        &self,
        command: TaxCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_tax_man(&command, &responder).await? {
            return Ok(());
        }
        defer(&responder, true).await?;
        let target = required_user_option(&command.options, "user")?;
        let tax = Arc::clone(&self.tax);
        let guild_id = command.guild_id;
        let target_id = target.id;
        let output = tokio::task::spawn_blocking(move || {
            let snapshot =
                tax.get_player_snapshot(target_id, guild_id, PLAYER_LEDGER_DEFAULT_LIMIT, 0)?;
            let total_entries = tax.count_ledger_entries(guild_id, Some(target_id))?;
            Ok::<_, String>((snapshot, total_entries))
        })
        .await
        .map_err(|error| format!("tax player task failed: {error}"))?;
        let (snapshot, total_entries) = match output {
            Ok(output) => output,
            Err(error) if error == "target_not_registered" => {
                return followup_ephemeral(
                    &responder,
                    "That user is not registered in this guild.",
                )
                .await;
            }
            Err(_) => {
                return followup_ephemeral(
                    &responder,
                    "Couldn't load that Tax Man player audit right now.",
                )
                .await;
            }
        };
        let embed = build_player_embed(
            &target,
            &snapshot,
            1,
            Some(total_entries),
            PLAYER_LEDGER_DEFAULT_LIMIT,
        );
        let view =
            (ledger_total_pages(total_entries, PLAYER_LEDGER_DEFAULT_LIMIT) > 1).then(|| {
                TaxPlayerLedgerView::new(
                    command.guild_id,
                    command.actor_id,
                    target,
                    PLAYER_LEDGER_DEFAULT_LIMIT,
                    1,
                    total_entries,
                )
            });
        let components = view.as_ref().map_or_else(Vec::new, |view| {
            pager_components(command.interaction_id, "player", view.buttons)
        });
        followup(
            &responder,
            InteractionResponse::message("")
                .embed(tax_embed(embed))
                .ephemeral()
                .action_rows(components),
        )
        .await?;
        if let Some(view) = view {
            self.store_view(command.interaction_id, StoredTaxView::player(view))?;
        }
        Ok(())
    }

    async fn fine(
        &self,
        command: TaxCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_tax_man(&command, &responder).await? {
            return Ok(());
        }
        defer(&responder, true).await?;
        let target = required_user_option(&command.options, "user")?;
        let amount = required_integer_option(&command.options, "amount")?;
        let reason = string_option(&command.options, "reason");
        let tax = Arc::clone(&self.tax);
        let permissions = self.permission_context(&command);
        let admin = self.admin_user_ids.clone();
        let tax_men = self.tax_man_user_ids.clone();
        let request = FineCommandRequest {
            target,
            guild_id: command.guild_id,
            actor_id: command.actor_id,
            amount,
            reason,
        };
        let response = tokio::task::spawn_blocking(move || {
            let authorization = TaxCommandAuthorization {
                permissions,
                admin_user_ids: &admin,
                tax_man_user_ids: &tax_men,
            };
            fine_command(tax.as_ref(), &authorization, request)
        })
        .await
        .map_err(|error| format!("tax fine task failed: {error}"))?;
        followup(&responder, command_response(response)).await
    }

    async fn resetcooldown(
        &self,
        command: TaxCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_tax_man(&command, &responder).await? {
            return Ok(());
        }
        defer(&responder, true).await?;
        let target = required_user_option(&command.options, "user")?;
        let tax = Arc::clone(&self.tax);
        let permissions = self.permission_context(&command);
        let admin = self.admin_user_ids.clone();
        let tax_men = self.tax_man_user_ids.clone();
        let request = ResetCooldownCommandRequest {
            target,
            guild_id: command.guild_id,
            actor_id: command.actor_id,
        };
        let response = tokio::task::spawn_blocking(move || {
            let authorization = TaxCommandAuthorization {
                permissions,
                admin_user_ids: &admin,
                tax_man_user_ids: &tax_men,
            };
            resetcooldown_command(tax.as_ref(), &authorization, request)
        })
        .await
        .map_err(|error| format!("tax reset-cooldown task failed: {error}"))?;
        followup(&responder, command_response(response)).await
    }

    async fn bankruptcy(
        &self,
        command: TaxCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_tax_man(&command, &responder).await? {
            return Ok(());
        }
        defer(&responder, true).await?;
        let target = required_user_option(&command.options, "user")?;
        let action = required_string_option(&command.options, "action")?;
        let action = match action.as_str() {
            "add" => BankruptcyAction::Add {
                games: integer_option(&command.options, "games")
                    .unwrap_or(self.bankruptcy_penalty_games),
            },
            "remove" => BankruptcyAction::Remove,
            _ => {
                return followup_ephemeral(&responder, "Unknown bankruptcy modifier action.").await;
            }
        };
        let tax = Arc::clone(&self.tax);
        let permissions = self.permission_context(&command);
        let admin = self.admin_user_ids.clone();
        let tax_men = self.tax_man_user_ids.clone();
        let request = BankruptcyCommandRequest {
            target,
            guild_id: command.guild_id,
            actor_id: command.actor_id,
            action,
            reason: string_option(&command.options, "reason"),
        };
        let response = tokio::task::spawn_blocking(move || {
            let authorization = TaxCommandAuthorization {
                permissions,
                admin_user_ids: &admin,
                tax_man_user_ids: &tax_men,
            };
            bankruptcy_command(tax.as_ref(), &authorization, request)
        })
        .await
        .map_err(|error| format!("tax bankruptcy task failed: {error}"))?;
        followup(&responder, command_response(response)).await
    }

    async fn ledger(
        &self,
        command: TaxCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_tax_man(&command, &responder).await? {
            return Ok(());
        }
        defer(&responder, true).await?;
        let tax = Arc::clone(&self.tax);
        let permissions = self.permission_context(&command);
        let admin = self.admin_user_ids.clone();
        let tax_men = self.tax_man_user_ids.clone();
        let request = LedgerCommandRequest {
            guild_id: command.guild_id,
            requester_id: command.actor_id,
            user: user_option(&command.options, "user")?,
            limit: usize::try_from(integer_option(&command.options, "limit").unwrap_or(10))
                .unwrap_or(10),
            page: usize::try_from(integer_option(&command.options, "page").unwrap_or(1))
                .unwrap_or(1),
        };
        let output = tokio::task::spawn_blocking(move || {
            let authorization = TaxCommandAuthorization {
                permissions,
                admin_user_ids: &admin,
                tax_man_user_ids: &tax_men,
            };
            ledger_command(tax.as_ref(), &authorization, request)
        })
        .await
        .map_err(|error| format!("tax ledger task failed: {error}"))?;
        let components = output.view.as_ref().map_or_else(Vec::new, |view| {
            pager_components(command.interaction_id, "ledger", view.buttons)
        });
        followup(
            &responder,
            command_response(output.response).action_rows(components),
        )
        .await?;
        if let Some(view) = output.view {
            self.store_view(command.interaction_id, StoredTaxView::ledger(view))?;
        }
        Ok(())
    }

    async fn handle_component(
        &self,
        custom_id: &str,
        actor_id: u64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let (kind, session_id, direction) = parse_component_id(custom_id)?;
        let Some(stored) = self.load_view(session_id)? else {
            // A timed-out discord.py View is no longer dispatched. Preserve
            // that no-ack behavior for stale buttons after timeout or restart.
            return Ok(());
        };
        if Instant::now() >= stored.expires_at() {
            self.remove_view(session_id)?;
            return Ok(());
        }
        // discord.py refreshes a View timeout before its ownership check and
        // callback, including denied clicks and callback failures.
        let stored = stored.refreshed();
        self.store_view(session_id, stored.clone())?;
        match stored {
            StoredTaxView::Ledger { expires_at, view } if kind == "ledger" => {
                if actor_id != view.requester_id {
                    return respond_ephemeral(
                        &responder,
                        "This ledger view belongs to another Tax Man.",
                    )
                    .await;
                }
                defer(&responder, false).await?;
                self.paginate_ledger(session_id, expires_at, view, direction, responder)
                    .await
            }
            StoredTaxView::Player { expires_at, view } if kind == "player" => {
                if actor_id != view.requester_id {
                    return respond_ephemeral(
                        &responder,
                        "This player audit view belongs to another Tax Man.",
                    )
                    .await;
                }
                defer(&responder, false).await?;
                self.paginate_player(session_id, expires_at, view, direction, responder)
                    .await
            }
            _ => Err("tax pager kind does not match stored view".into()),
        }
    }

    async fn paginate_ledger(
        &self,
        session_id: u64,
        expires_at: Instant,
        mut view: TaxLedgerView,
        direction: PageDirection,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let requested_page = direction.apply(view.current_page);
        let tax = Arc::clone(&self.tax);
        let mut candidate = view.clone();
        let result = tokio::task::spawn_blocking(move || {
            let page = candidate.show_page(tax.as_ref(), requested_page);
            (candidate, page)
        })
        .await
        .map_err(|error| format!("tax ledger pagination task failed: {error}"))?;
        let (candidate, page) = result;
        if let Some(error) = page.ephemeral_error {
            return followup_ephemeral(&responder, &error).await;
        }
        let Some(embed) = page.edited_embed else {
            return followup_ephemeral(&responder, "Couldn't load that ledger page right now.")
                .await;
        };
        view = candidate;
        let response = InteractionResponse::message("")
            .embed(tax_embed(embed))
            .action_rows(pager_components(session_id, "ledger", view.buttons));
        self.store_view(session_id, StoredTaxView::ledger_at(view, expires_at))?;
        edit_original(&responder, response).await
    }

    async fn paginate_player(
        &self,
        session_id: u64,
        expires_at: Instant,
        view: TaxPlayerLedgerView,
        direction: PageDirection,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let requested_page = direction.apply(view.current_page);
        let tax = Arc::clone(&self.tax);
        let result = tokio::task::spawn_blocking(move || {
            let total_entries = tax
                .count_ledger_entries(view.guild_id, Some(view.user.id))
                .map_err(|_| PlayerPageError::Database)?;
            let current_page = clamp_ledger_page(requested_page, view.limit, total_entries);
            let snapshot = tax
                .get_player_snapshot(
                    view.user.id,
                    view.guild_id,
                    view.limit,
                    ledger_offset(current_page, view.limit),
                )
                .map_err(|error| {
                    if error == "target_not_registered" {
                        PlayerPageError::TargetNotRegistered
                    } else {
                        PlayerPageError::Database
                    }
                })?;
            let embed = build_player_embed(
                &view.user,
                &snapshot,
                current_page,
                Some(total_entries),
                view.limit,
            );
            let candidate = TaxPlayerLedgerView::new(
                view.guild_id,
                view.requester_id,
                view.user,
                view.limit,
                current_page,
                total_entries,
            );
            Ok::<_, PlayerPageError>((candidate, embed))
        })
        .await
        .map_err(|error| format!("tax player pagination task failed: {error}"))?;
        let (view, embed) = match result {
            Ok(result) => result,
            Err(PlayerPageError::TargetNotRegistered) => {
                return followup_ephemeral(
                    &responder,
                    "That user is not registered in this guild.",
                )
                .await;
            }
            Err(PlayerPageError::Database) => {
                return followup_ephemeral(
                    &responder,
                    "Couldn't load that player ledger page right now.",
                )
                .await;
            }
        };
        let response = InteractionResponse::message("")
            .embed(tax_embed(embed))
            .action_rows(pager_components(session_id, "player", view.buttons));
        self.store_view(session_id, StoredTaxView::player_at(view, expires_at))?;
        edit_original(&responder, response).await
    }

    fn store_view(
        &self,
        session_id: u64,
        view: StoredTaxView,
    ) -> Result<(), InteractionHandlerError> {
        let expires_at = view.expires_at();
        self.views
            .lock()
            .map_err(|_| InteractionHandlerError::from("tax view registry lock was poisoned"))?
            .insert(session_id, view);

        let views = Arc::clone(&self.views);
        tokio::spawn(async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(expires_at)).await;
            let Ok(mut views) = views.lock() else {
                return;
            };
            let is_current_expiration = views
                .get(&session_id)
                .is_some_and(|stored| stored.expires_at() == expires_at);
            if is_current_expiration {
                views.remove(&session_id);
            }
        });
        Ok(())
    }

    fn load_view(&self, session_id: u64) -> Result<Option<StoredTaxView>, InteractionHandlerError> {
        Ok(self
            .views
            .lock()
            .map_err(|_| InteractionHandlerError::from("tax view registry lock was poisoned"))?
            .get(&session_id)
            .cloned())
    }

    fn remove_view(&self, session_id: u64) -> Result<(), InteractionHandlerError> {
        self.views
            .lock()
            .map_err(|_| InteractionHandlerError::from("tax view registry lock was poisoned"))?
            .remove(&session_id);
        Ok(())
    }
}

enum PlayerPageError {
    TargetNotRegistered,
    Database,
}

#[derive(Clone)]
struct SqliteTaxPort {
    repository: TaxRepository,
    fine_cooldown_seconds: i64,
}

impl SqliteTaxPort {
    fn guild_snapshot(&self, guild_id: i64) -> Result<TaxGuildSnapshot, String> {
        self.repository
            .guild_snapshot(guild_id, Utc::now().timestamp())
            .map_err(|error| error.to_string())
    }

    fn source_totals(
        &self,
        guild_id: i64,
        limit: usize,
    ) -> Result<Vec<TaxLedgerSourceTotal>, String> {
        self.repository
            .source_totals(guild_id, limit)
            .map_err(|error| error.to_string())
    }
}

impl LedgerReadPort for SqliteTaxPort {
    type Error = String;

    fn count_ledger_entries(
        &self,
        guild_id: u64,
        user_id: Option<u64>,
    ) -> Result<usize, Self::Error> {
        self.repository
            .count_ledger_entries(
                signed_id(guild_id, "guild").map_err(|error| error.to_string())?,
                user_id
                    .map(|id| signed_id(id, "user").map_err(|error| error.to_string()))
                    .transpose()?,
            )
            .map_err(|error| error.to_string())
    }

    fn get_recent_ledger(
        &self,
        guild_id: u64,
        limit: usize,
        offset: usize,
        user_id: Option<u64>,
    ) -> Result<Vec<LedgerRow>, Self::Error> {
        self.repository
            .recent_ledger(
                signed_id(guild_id, "guild").map_err(|error| error.to_string())?,
                limit,
                offset,
                user_id
                    .map(|id| signed_id(id, "user").map_err(|error| error.to_string()))
                    .transpose()?,
            )
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(ledger_entry)
            .collect()
    }
}

impl PlayerAuditReadPort for SqliteTaxPort {
    fn get_player_snapshot(
        &self,
        user_id: u64,
        guild_id: u64,
        ledger_limit: usize,
        ledger_offset: usize,
    ) -> Result<PlayerSnapshot, Self::Error> {
        let user_id = signed_id(user_id, "user").map_err(|error| error.to_string())?;
        let guild_id = signed_id(guild_id, "guild").map_err(|error| error.to_string())?;
        let snapshot = self
            .repository
            .player_snapshot(
                user_id,
                guild_id,
                ledger_limit,
                ledger_offset,
                Utc::now().timestamp(),
            )
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "target_not_registered".to_owned())?;
        player_snapshot(snapshot)
    }
}

impl TaxEnforcementPort for SqliteTaxPort {
    type Error = String;

    fn levy_fine_atomic(&self, request: FineRequest) -> Result<FineResult, Self::Error> {
        if request.amount <= 0 {
            return Ok(FineResult::InvalidAmount);
        }
        let discord_id = signed_id(request.user_id, "user").map_err(|error| error.to_string())?;
        let guild_id = signed_id(request.guild_id, "guild").map_err(|error| error.to_string())?;
        let outcome = self
            .repository
            .levy_fine_atomic(&TaxFineRequest {
                discord_id,
                guild_id,
                amount: request.amount,
                actor_id: signed_id(request.actor_id, "actor")
                    .map_err(|error| error.to_string())?,
                reason: request.reason,
                cooldown_seconds: self.fine_cooldown_seconds,
                now: Utc::now().timestamp(),
            })
            .map_err(|error| error.to_string())?;
        Ok(match outcome {
            TaxFineOutcome::Ok {
                requested_amount,
                applied_amount,
                balance_before,
                balance_after,
                next_fine_at,
                ..
            } => FineResult::Ok {
                requested_amount,
                applied_amount,
                balance_before,
                balance_after,
                next_fine_at: Some(next_fine_at),
            },
            TaxFineOutcome::Cooldown { next_fine_at } => FineResult::Cooldown { next_fine_at },
            TaxFineOutcome::TargetNotRegistered => FineResult::TargetNotRegistered,
            TaxFineOutcome::InvalidAmount => FineResult::InvalidAmount,
        })
    }

    fn reset_fine_cooldown_atomic(
        &self,
        request: ResetFineCooldownRequest,
    ) -> Result<ResetFineCooldownResult, Self::Error> {
        self.repository
            .reset_fine_cooldown_atomic(
                signed_id(request.user_id, "user").map_err(|error| error.to_string())?,
                signed_id(request.guild_id, "guild").map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())
            .map(|result| match result {
                Some(had_cooldown) => ResetFineCooldownResult::Ok { had_cooldown },
                None => ResetFineCooldownResult::TargetNotRegistered,
            })
    }

    fn change_bankruptcy_modifier_atomic(
        &self,
        request: BankruptcyRequest,
    ) -> Result<BankruptcyResult, Self::Error> {
        let action = match request.action {
            BankruptcyAction::Add { games } => TaxBankruptcyAction::Add { games },
            BankruptcyAction::Remove => TaxBankruptcyAction::Remove,
        };
        self.repository
            .change_bankruptcy_modifier_atomic(TaxBankruptcyRequest {
                discord_id: signed_id(request.user_id, "user")
                    .map_err(|error| error.to_string())?,
                guild_id: signed_id(request.guild_id, "guild")
                    .map_err(|error| error.to_string())?,
                action,
            })
            .map_err(|error| error.to_string())
            .map(|outcome| match outcome {
                TaxBankruptcyOutcome::Added {
                    games,
                    previous_games,
                    penalty_games_remaining,
                } => BankruptcyResult::Added {
                    games,
                    previous_games,
                    penalty_games_remaining,
                },
                TaxBankruptcyOutcome::Removed {
                    previous_games,
                    penalty_games_remaining,
                } => BankruptcyResult::Removed {
                    previous_games,
                    penalty_games_remaining,
                },
                TaxBankruptcyOutcome::TargetNotRegistered => BankruptcyResult::TargetNotRegistered,
                TaxBankruptcyOutcome::InvalidGames => BankruptcyResult::InvalidGames,
            })
    }
}

#[derive(Clone)]
struct PersistentVanityPort {
    service: Arc<PersistentVanityTaxService>,
    rate_fraction: f64,
}

impl VanityTaxPort for PersistentVanityPort {
    type Error = String;

    fn tax_rate_basis_points(&self) -> i64 {
        (self.rate_fraction * 10_000.0).round() as i64
    }

    fn tax_rate_fraction(&self) -> f64 {
        self.rate_fraction
    }

    fn eligibility_status(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<VanityEligibility, Self::Error> {
        Ok(vanity_eligibility(self.service.eligibility_status(
            signed_id(guild_id, "guild").map_err(|error| error.to_string())?,
            signed_id(user_id, "user").map_err(|error| error.to_string())?,
        )))
    }

    fn set_manual_taxation_atomic(
        &self,
        request: ManualTaxationRequest,
    ) -> Result<VanityEligibility, Self::Error> {
        let guild_id = signed_id(request.guild_id, "guild").map_err(|error| error.to_string())?;
        let user_id = signed_id(request.user_id, "user").map_err(|error| error.to_string())?;
        self.service
            .set_manual_taxation(
                guild_id,
                user_id,
                request.enforced,
                signed_id(request.actor_id, "actor").map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        Ok(vanity_eligibility(
            self.service.eligibility_status(guild_id, user_id),
        ))
    }
}

fn vanity_eligibility(value: PersistentVanityEligibility) -> VanityEligibility {
    match value {
        PersistentVanityEligibility::ManualTaxation => VanityEligibility::ManualTaxation,
        PersistentVanityEligibility::Taxable => VanityEligibility::Taxable,
        PersistentVanityEligibility::NicknameExemption => VanityEligibility::NicknameExemption,
        PersistentVanityEligibility::Unknown => VanityEligibility::Unknown,
    }
}

#[derive(Clone)]
struct SqliteEconomyStatusPort {
    repository: EconomyEventRepository,
    config: EconomyEventConfig,
}

impl EconomyStatusPort for SqliteEconomyStatusPort {
    type Error = String;

    fn get_policy_status(&self, guild_id: u64) -> Result<PolicyStatus, Self::Error> {
        let guild_id = signed_id(guild_id, "guild").map_err(|error| error.to_string())?;
        let now = Utc::now().timestamp();
        let policy = self
            .repository
            .ensure_policy_state(
                Some(guild_id),
                if self.config.enabled {
                    PolicyMode::Normal
                } else {
                    PolicyMode::Disabled
                },
                self.config.normal_annual_rate,
                self.config.inflation_ceiling,
                now,
            )
            .map_err(|error| error.to_string())?;
        let sheet = self
            .repository
            .capture_balance_sheet(Some(guild_id))
            .map_err(|error| error.to_string())?;
        let latest = self
            .repository
            .get_latest_snapshot(Some(guild_id))
            .map_err(|error| error.to_string())?;
        let trends = self
            .repository
            .get_monetary_trends(Some(guild_id), sheet.monetary_stock, now)
            .map_err(|error| error.to_string())?;
        let flows = self
            .repository
            .get_flow_indicators(
                Some(guild_id),
                sheet.monetary_stock,
                sheet.player_count,
                now,
            )
            .map_err(|error| error.to_string())?;
        let distribution = self
            .repository
            .get_distribution_indicators(Some(guild_id))
            .map_err(|error| error.to_string())?;
        let planner = EconomyEventService::new(
            MemoryEventStore::default(),
            MemoryEconomy::default(),
            FixedClock::new(now),
            DeterministicEventRng,
            self.config.clone(),
        );
        let event_date = planner
            .event_date_for_timestamp(now)
            .map_err(|error| error.to_string())?;
        let event = self
            .repository
            .get_event_for_date(Some(guild_id), &event_date)
            .map_err(|error| error.to_string())?;
        let mut monetary_trends = BTreeMap::new();
        for (days, label) in [(1, "1d"), (3, "3d"), (7, "7d")] {
            if let Some(Some(trend)) = trends.get(&days) {
                monetary_trends.insert(
                    label,
                    MonetaryTrend {
                        elapsed_days: trend.elapsed_days,
                        period_rate: trend.period_rate,
                        daily_rate: trend.daily_rate,
                        change_jc: trend.change_jc,
                        average_daily_change_jc: trend.average_daily_change_jc,
                    },
                );
            }
        }
        let mut flow_indicators = BTreeMap::new();
        for (days, label) in [(3, "3d"), (7, "7d")] {
            if let Some(flow) = flows.get(&days) {
                flow_indicators.insert(
                    label,
                    FlowIndicator {
                        average_daily_net_jc: flow.average_daily_net_jc,
                        average_daily_gross_jc: flow.average_daily_gross_jc,
                        daily_turnover_rate: flow.daily_turnover_rate,
                        active_players: flow.active_players,
                        active_player_rate: flow.active_player_rate,
                    },
                );
            }
        }
        Ok(PolicyStatus {
            policy: EconomyPolicy {
                mode: policy.mode.as_str().to_owned(),
                target_annual_rate: policy.target_annual_rate,
                inflation_ceiling: policy.inflation_ceiling,
            },
            balance_sheet: BalanceSheet {
                monetary_stock: sheet.monetary_stock,
                player_wallets: sheet.player_wallets,
                average_wallet: sheet.average_wallet,
                reserve_available: sheet.reserve_available,
                reserve_locked: sheet.reserve_locked,
                prediction_open_cash: sheet.prediction_open_cash,
                wager_escrow: sheet.wager_escrow,
                player_count: sheet.player_count,
                positive_wallets: sheet.positive_wallets,
                visible_debt: sheet.visible_debt,
                reserve_next_match_pot: sheet.reserve_next_match_pot,
            },
            annualized_30d: latest.as_ref().and_then(|snapshot| snapshot.annualized_30d),
            annualized_90d: latest.as_ref().and_then(|snapshot| snapshot.annualized_90d),
            monetary_trends,
            flow_indicators,
            distribution: Distribution {
                median_positive_wallet: distribution.median_positive_wallet,
                top_decile_share: distribution.top_decile_share,
                gini: distribution.gini,
            },
            event: event.map(economy_event),
        })
    }
}

struct CatalogSpellIcons;

impl SpellIconPort for CatalogSpellIcons {
    type Error = Infallible;

    fn get_ability_icon_url(&self, event_name: &str) -> Result<Option<String>, Self::Error> {
        Ok(cama_app::economy_event_service::EVENT_CATALOG
            .iter()
            .copied()
            .find(|event| event.name.eq_ignore_ascii_case(event_name))
            .map(|event| event.ability_icon_url()))
    }
}

fn economy_event(event: cama_db::economy_event_repository::EconomyEvent) -> EconomyEvent {
    let reserve_voting_disabled = event
        .reserve_voting_disabled_explicit
        .then_some(event.effects.reserve_voting_disabled);
    EconomyEvent {
        name: event.name,
        severity: u8::try_from(event.severity.clamp(1, 5)).unwrap_or(1),
        direction: match event.direction {
            EventDirection::Deflationary => EconomyDirection::Deflationary,
            EventDirection::Neutral => EconomyDirection::Neutral,
            EventDirection::Boon => EconomyDirection::Boon,
        },
        announcement: event.announcement,
        effects: EconomyEffects {
            reward_multiplier: event.effects.reward_multiplier,
            gamba_win_multiplier: event.effects.gamba_win_multiplier,
            gamba_loss_multiplier: event.effects.gamba_loss_multiplier,
            bet_payout_multiplier: event.effects.bet_payout_multiplier,
            prediction_payout_multiplier: event.effects.prediction_payout_multiplier,
            prediction_depth_multiplier: event.effects.prediction_depth_multiplier,
            prediction_spread_ticks_delta: i32::try_from(
                event.effects.prediction_spread_ticks_delta,
            )
            .unwrap_or_default(),
            reserve_voting_disabled,
            reserve_burn_jc: event.effects.reserve_burn_jc,
            wallet_burn_jc: event.effects.wallet_burn_jc,
            reserve_release_jc: event.effects.reserve_release_jc,
        },
        forecast_flow_jc: event.forecast_flow_jc,
        target_effect_jc: event.target_effect_jc,
        expected_effect_jc: event.expected_effect_jc,
        direct_effect_jc: event.direct_effect_jc,
        ends_at: Some(event.ends_at),
    }
}

fn ledger_entry(entry: TaxLedgerEntry) -> Result<LedgerRow, String> {
    let account = if entry.account_type == "nonprofit" {
        LedgerAccount::Reserve
    } else {
        LedgerAccount::Player(
            entry
                .account_id
                .and_then(|id| u64::try_from(id).ok())
                .unwrap_or_default(),
        )
    };
    Ok(LedgerRow {
        ledger_id: entry.ledger_id,
        guild_id: u64::try_from(entry.guild_id).map_err(|_| "invalid ledger guild id")?,
        account,
        delta: entry.delta,
        balance_before: entry.balance_before,
        balance_after: entry.balance_after,
        source: entry.source,
        actor_id: entry.actor_id.and_then(|id| u64::try_from(id).ok()),
        related_type: entry.related_type,
        related_id: entry.related_id,
        reason: entry.reason,
        created_at: entry.created_at,
    })
}

fn player_snapshot(
    snapshot: cama_db::tax_repository::TaxPlayerSnapshot,
) -> Result<PlayerSnapshot, String> {
    let positions = snapshot
        .prediction_exposure
        .positions
        .into_iter()
        .map(|position| PredictionPosition {
            prediction_id: position.prediction_id,
            current_price: position.current_price,
            question: position.question,
            yes_contracts: position.yes_contracts,
            yes_cost_basis: position.yes_cost_basis,
            no_contracts: position.no_contracts,
            no_cost_basis: position.no_cost_basis,
            ev: position.ev,
        })
        .collect();
    let summary = snapshot.prediction_exposure.summary;
    Ok(PlayerSnapshot {
        balance: snapshot.balance,
        visible_debt: snapshot.visible_debt,
        loan_principal: snapshot.loan_principal,
        loan_fee: snapshot.loan_fee,
        loan_total: snapshot.loan_total,
        total_loans_taken: snapshot.total_loans_taken,
        bankruptcy_count: snapshot.bankruptcy_count,
        penalty_games_remaining: snapshot.penalty_games_remaining,
        dark_bargain_count: snapshot.dark_bargain_count,
        dark_bargain_due: snapshot.dark_bargain_due,
        effective_obligations: snapshot.effective_obligations,
        prediction_exposure: PredictionExposure {
            summary: PredictionSummary {
                cost_basis: summary.cost_basis,
                expected_payout: summary.expected_payout,
                ev: summary.ev,
                max_payout: summary.max_payout,
            },
            positions,
        },
        recent_ledger: snapshot
            .recent_ledger
            .into_iter()
            .map(ledger_entry)
            .collect::<Result<_, _>>()?,
    })
}

fn build_audit_embed(
    snapshot: &TaxGuildSnapshot,
    source_totals: &[TaxLedgerSourceTotal],
) -> InteractionEmbed {
    let summary = &snapshot.prediction_exposure.summary;
    InteractionEmbed::titled("Tax Man Guild Audit")
        .color(0xF1C40F)
        .field(
            "Balances",
            format!(
                "Players: {}\nTotal balance: {}\nPositive balance: {}\nVisible debt: {}\nBroke players: {}",
                grouped(snapshot.players),
                format_jc(snapshot.total_balance),
                format_jc(snapshot.positive_balance),
                format_jc(snapshot.visible_debt),
                grouped(snapshot.broke_players),
            ),
            true,
        )
        .field(
            "Jopacoin Reserve",
            format!(
                "Available: {}\nReserved: {}",
                format_jc(snapshot.nonprofit_available),
                format_jc(snapshot.nonprofit_reserved),
            ),
            true,
        )
        .field(
            "Obligations",
            format!(
                "Loans: {} ({} borrowers)\nDark Bargains: {} ({} active)\nPending bets: {} ({} bets)\nOpen markets: {}",
                format_jc(snapshot.loan_principal.saturating_add(snapshot.loan_fee)),
                grouped(snapshot.loan_borrowers),
                format_jc(snapshot.dark_bargain_due),
                grouped(snapshot.dark_bargain_count),
                format_jc(snapshot.pending_bet_effective_stake),
                grouped(snapshot.pending_bet_count),
                grouped(snapshot.open_prediction_markets),
            ),
            false,
        )
        .field(
            "Prediction Markets",
            format!(
                "Markets: {}\nHolders: {}\nCost basis: {}\nExpected payout: {}\nEV to holders: {}\nWorst-case payout: {}\nBook depth: {} contracts",
                grouped(summary.open_markets),
                grouped(summary.holder_count),
                format_jc(summary.cost_basis),
                format_jc(summary.expected_payout),
                format_signed_jc(summary.ev_to_holders),
                format_jc(summary.worst_case_payout),
                grouped(summary.book_contracts),
            ),
            false,
        )
        .field(
            "Prediction Detail",
            format_prediction_markets(&snapshot.prediction_exposure.markets),
            false,
        )
        .field(
            "Ledger Sources",
            format_source_totals(source_totals),
            false,
        )
        .footer("Tax Man audit only")
}

fn format_prediction_markets(markets: &[cama_db::tax_repository::TaxPredictionMarket]) -> String {
    if markets.is_empty() {
        return "No open prediction markets.".to_owned();
    }
    markets
        .iter()
        .take(5)
        .map(|market| {
            format!(
                "#{} `{}%` {}\n  YES {} / NO {}, cost {}, EV {}, worst {}, book {}/{}",
                market.prediction_id,
                market.current_price,
                short_text(&market.question, 48),
                grouped(market.yes_contracts),
                grouped(market.no_contracts),
                format_jc(market.cost_basis),
                format_signed_jc(market.ev_to_holders),
                format_jc(market.worst_case_payout),
                market
                    .top_yes_bid
                    .map_or_else(|| "-".to_owned(), |value| value.to_string()),
                market
                    .top_yes_ask
                    .map_or_else(|| "-".to_owned(), |value| value.to_string()),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_source_totals(rows: &[TaxLedgerSourceTotal]) -> String {
    if rows.is_empty() {
        return "No ledger entries yet.".to_owned();
    }
    rows.iter()
        .take(8)
        .map(|row| {
            format!(
                "`{}`: {} entries, net {}",
                row.source,
                grouped(row.entry_count),
                format_signed_jc(row.net_delta),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn short_text(value: &str, limit: usize) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.chars().count() <= limit {
        value
    } else {
        let mut result = value
            .chars()
            .take(limit.saturating_sub(1))
            .collect::<String>();
        result.push_str("...");
        result
    }
}

fn format_jc(amount: i64) -> String {
    format!("{} {JOPACOIN_EMOTE}", grouped(amount))
}

fn format_signed_jc(amount: i64) -> String {
    let value = if amount > 0 {
        format!("+{}", grouped(amount))
    } else {
        grouped(amount)
    };
    format!("{value} {JOPACOIN_EMOTE}")
}

fn grouped(amount: i64) -> String {
    let negative = amount < 0;
    let digits = amount.unsigned_abs().to_string();
    let first = digits.len() % 3;
    let mut result = String::new();
    if first != 0 {
        result.push_str(&digits[..first]);
    }
    for chunk in digits.as_bytes()[first..].chunks(3) {
        if !result.is_empty() {
            result.push(',');
        }
        result.push_str(std::str::from_utf8(chunk).expect("decimal digits are UTF-8"));
    }
    if negative {
        format!("-{result}")
    } else {
        result
    }
}

fn tax_embed(embed: TaxEmbed) -> InteractionEmbed {
    let model = embed.model;
    let mut result = InteractionEmbed {
        title: model.title,
        description: model.description,
        color: Some(embed.color),
        footer: model.footer,
        thumbnail_url: embed.thumbnail_url,
        author_name: model.author,
        ..InteractionEmbed::default()
    };
    result.fields = model
        .fields
        .into_iter()
        .map(|field| crate::registration::InteractionEmbedField {
            name: field.name,
            value: field.value,
            inline: field.inline,
        })
        .collect();
    result
}

fn command_response(response: CommandResponse) -> InteractionResponse {
    let mut result = InteractionResponse::message(response.content.unwrap_or_default());
    result.ephemeral = response.ephemeral;
    if let Some(embed) = response.embed {
        result.embeds.push(tax_embed(embed));
    }
    result
}

fn pager_components(
    session_id: u64,
    kind: &str,
    buttons: cama_app::tax_commands::PaginationButtons,
) -> Vec<InteractionActionRow> {
    vec![InteractionActionRow::buttons(vec![
        InteractionButton::new(format!("tax:{kind}:{session_id}:previous"), "Previous")
            .style(InteractionButtonStyle::Secondary)
            .disabled(buttons.previous_disabled),
        InteractionButton::new(format!("tax:{kind}:{session_id}:next"), "Next")
            .style(InteractionButtonStyle::Secondary)
            .disabled(buttons.next_disabled),
    ])]
}

#[derive(Clone, Copy)]
enum PageDirection {
    Previous,
    Next,
}

impl PageDirection {
    fn apply(self, current: usize) -> usize {
        match self {
            Self::Previous => current.saturating_sub(1).max(1),
            Self::Next => current.saturating_add(1),
        }
    }
}

fn parse_component_id(
    custom_id: &str,
) -> Result<(&str, u64, PageDirection), InteractionHandlerError> {
    let mut parts = custom_id.split(':');
    if parts.next() != Some("tax") {
        return Err("invalid tax component prefix".into());
    }
    let kind = parts
        .next()
        .filter(|kind| matches!(*kind, "ledger" | "player"))
        .ok_or_else(|| InteractionHandlerError::from("invalid tax component kind"))?;
    let session_id = parts
        .next()
        .ok_or_else(|| InteractionHandlerError::from("missing tax component session"))?
        .parse()
        .map_err(|_| InteractionHandlerError::from("invalid tax component session"))?;
    let direction = match parts.next() {
        Some("previous") => PageDirection::Previous,
        Some("next") => PageDirection::Next,
        _ => return Err("invalid tax component direction".into()),
    };
    if parts.next().is_some() {
        return Err("invalid tax component suffix".into());
    }
    Ok((kind, session_id, direction))
}

fn nested_subcommand(
    options: &[InteractionOption],
) -> Result<(String, Vec<InteractionOption>), InteractionHandlerError> {
    options
        .iter()
        .find_map(|option| match &option.value {
            InteractionValue::Subcommand(children) => Some((option.name.clone(), children.clone())),
            _ => None,
        })
        .ok_or_else(|| "the /tax subcommand is missing".into())
}

fn user_option(
    options: &[InteractionOption],
    name: &str,
) -> Result<Option<UserRef>, InteractionHandlerError> {
    options
        .iter()
        .find(|option| option.name == name)
        .map(|option| match &option.value {
            InteractionValue::User {
                id, display_name, ..
            } => {
                let display_name = display_name.clone().unwrap_or_else(|| format!("User {id}"));
                Ok(UserRef {
                    id: *id,
                    name: display_name.to_lowercase(),
                    display_name,
                })
            }
            _ => Err(format!("/{name} must be a Discord user").into()),
        })
        .transpose()
}

fn required_user_option(
    options: &[InteractionOption],
    name: &str,
) -> Result<UserRef, InteractionHandlerError> {
    user_option(options, name)?
        .ok_or_else(|| InteractionHandlerError::from(format!("/tax requires {name}")))
}

fn integer_option(options: &[InteractionOption], name: &str) -> Option<i64> {
    options.iter().find_map(|option| {
        (option.name == name)
            .then_some(&option.value)
            .and_then(|value| match value {
                InteractionValue::Integer(value) => Some(*value),
                _ => None,
            })
    })
}

fn required_integer_option(
    options: &[InteractionOption],
    name: &str,
) -> Result<i64, InteractionHandlerError> {
    integer_option(options, name)
        .ok_or_else(|| InteractionHandlerError::from(format!("/tax requires {name}")))
}

fn string_option(options: &[InteractionOption], name: &str) -> Option<String> {
    options.iter().find_map(|option| {
        (option.name == name)
            .then_some(&option.value)
            .and_then(|value| match value {
                InteractionValue::String(value) => Some(value.clone()),
                _ => None,
            })
    })
}

fn required_string_option(
    options: &[InteractionOption],
    name: &str,
) -> Result<String, InteractionHandlerError> {
    string_option(options, name)
        .ok_or_else(|| InteractionHandlerError::from(format!("/tax requires {name}")))
}

fn boolean_option(options: &[InteractionOption], name: &str) -> Option<bool> {
    options.iter().find_map(|option| {
        (option.name == name)
            .then_some(&option.value)
            .and_then(|value| match value {
                InteractionValue::Boolean(value) => Some(*value),
                _ => None,
            })
    })
}

fn signed_id(value: u64, label: &str) -> Result<i64, InteractionHandlerError> {
    i64::try_from(value).map_err(|_| {
        InteractionHandlerError::from(format!("Discord {label} ID exceeds SQLite range"))
    })
}

async fn defer(
    responder: &Arc<dyn InteractionResponder>,
    ephemeral: bool,
) -> Result<(), InteractionHandlerError> {
    responder
        .defer(ephemeral)
        .await
        .map_err(|error| InteractionHandlerError::from(error.to_string()))
}

async fn respond(
    responder: &Arc<dyn InteractionResponder>,
    response: InteractionResponse,
) -> Result<(), InteractionHandlerError> {
    responder
        .respond(response)
        .await
        .map_err(|error| InteractionHandlerError::from(error.to_string()))
}

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    message: &str,
) -> Result<(), InteractionHandlerError> {
    respond(responder, InteractionResponse::message(message).ephemeral()).await
}

async fn followup(
    responder: &Arc<dyn InteractionResponder>,
    response: InteractionResponse,
) -> Result<(), InteractionHandlerError> {
    responder
        .followup(response)
        .await
        .map_err(|error| InteractionHandlerError::from(error.to_string()))
}

async fn followup_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    message: &str,
) -> Result<(), InteractionHandlerError> {
    followup(responder, InteractionResponse::message(message).ephemeral()).await
}

async fn edit_original(
    responder: &Arc<dyn InteractionResponder>,
    response: InteractionResponse,
) -> Result<(), InteractionHandlerError> {
    responder
        .edit_original(response)
        .await
        .map_err(|error| InteractionHandlerError::from(error.to_string()))
}

impl From<TaxRepositoryError> for InteractionHandlerError {
    fn from(error: TaxRepositoryError) -> Self {
        Self::from(error.to_string())
    }
}

#[cfg(all(test, feature = "runtime-test-core"))]
#[path = "tax_provider/tests.rs"]
mod tests;
