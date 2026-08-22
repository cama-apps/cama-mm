use std::sync::Mutex;

use cama_app::service_container::{ServiceContainer, ServiceContainerOptions};
use rusqlite::{Connection, params};
use tempfile::TempDir;

use super::*;
use crate::registration::{InteractionResponseError, Registry};
use crate::test_support::initialize_test_database as initialize_or_migrate;

const GUILD: u64 = 42;
const TAX_MAN: u64 = 99;

struct Fixture {
    _directory: TempDir,
    path: std::path::PathBuf,
    provider: TaxRegistrationProvider,
}

impl Fixture {
    fn migrated() -> Self {
        Self::migrated_with_config(config())
    }

    fn migrated_with_config(config: ApplicationConfig) -> Self {
        let directory = tempfile::tempdir().expect("temporary tax provider directory");
        let path = directory.path().join("cama.db");
        initialize_or_migrate(&path).expect("migrate tax provider database");
        let mut container = ServiceContainer::new(
            &path,
            ServiceContainerOptions {
                admin_user_ids: config.identities.admin_user_ids.clone(),
                vanity_tax_rate: config.values.vanity_tax_rate,
                ..ServiceContainerOptions::default()
            },
        );
        container.initialize();
        let vanity = Arc::clone(
            &container
                .components()
                .expect("tax service components")
                .vanity_tax_service,
        );
        let provider = TaxRegistrationProvider::new(&path, &config, vanity);
        Self {
            _directory: directory,
            path,
            provider,
        }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(&self.path).expect("open provider database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("production foreign-key mode");
        connection
    }

    fn registry(&self) -> Registry {
        let mut builder = RegistryBuilder::default();
        builder
            .add_provider(&self.provider)
            .expect("register tax provider");
        builder.build()
    }

    fn player(&self, id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,jopacoin_balance,lowest_balance_ever
                 ) VALUES (?1,?2,?3,?4,?4)",
                params![
                    id,
                    i64::try_from(GUILD).unwrap(),
                    format!("Player {id}"),
                    balance
                ],
            )
            .expect("insert provider player");
    }
}

#[derive(Default)]
struct Responder {
    immediate: Mutex<Vec<InteractionResponse>>,
    defers: Mutex<Vec<bool>>,
    followups: Mutex<Vec<InteractionResponse>>,
    edits: Mutex<Vec<InteractionResponse>>,
}

#[async_trait]
impl InteractionResponder for Responder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.immediate.lock().expect("immediate").push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.defers.lock().expect("defers").push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.followups.lock().expect("followups").push(response);
        Ok(())
    }

    async fn edit_original(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.edits.lock().expect("edits").push(response);
        Ok(())
    }
}

fn config() -> ApplicationConfig {
    config_with_vanity_rate("0.1")
}

fn config_with_vanity_rate(vanity_rate: &str) -> ApplicationConfig {
    ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some("99".to_owned()),
        "TAX_MAN_USER_IDS" => Some("98".to_owned()),
        "ECONOMY_EVENTS_ENABLED" => Some("false".to_owned()),
        "TAX_FINE_COOLDOWN_SECONDS" => Some("100".to_owned()),
        "BANKRUPTCY_PENALTY_GAMES" => Some("3".to_owned()),
        "VANITY_TAX_RATE" => Some(vanity_rate.to_owned()),
        _ => None,
    })
    .expect("tax provider configuration")
}

fn command(
    actor: u64,
    guild: Option<u64>,
    subcommand: &str,
    options: Vec<InteractionOption>,
    interaction_id: u64,
) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id,
        name: "tax".to_owned(),
        user_id: actor,
        user_display_name: format!("User {actor}"),
        guild_id: guild,
        channel_id: Some(7),
        member_permissions: None,
        options: vec![InteractionOption {
            name: subcommand.to_owned(),
            value: InteractionValue::Subcommand(options),
        }],
    }
}

fn user(name: &str, id: u64) -> InteractionOption {
    InteractionOption {
        name: name.to_owned(),
        value: InteractionValue::User {
            id,
            display_name: Some(format!("Player {id}")),
            is_bot: Some(false),
        },
    }
}

#[test]
fn schema_has_all_nine_exact_subcommands_and_options() {
    let fixture = Fixture::migrated();
    let registry = fixture.registry();
    let command = registry.commands().next().expect("tax command");
    assert_eq!(command.name, "tax");
    assert_eq!(command.description, "Jopacoin economy and Tax Man tools");
    assert_eq!(
        command
            .options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        [
            "audit",
            "policy",
            "event",
            "vanity",
            "player",
            "fine",
            "resetcooldown",
            "bankruptcy",
            "ledger",
        ]
    );
    assert_eq!(
        command.options[3].description,
        "Check the nickname vanity tax (10% of profits)"
    );
    let option_shapes = command
        .options
        .iter()
        .map(|subcommand| {
            (
                subcommand.name.as_str(),
                subcommand
                    .options
                    .iter()
                    .map(|option| (option.name.as_str(), option.kind, option.required))
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        option_shapes,
        [
            ("audit", vec![]),
            ("policy", vec![]),
            ("event", vec![]),
            (
                "vanity",
                vec![
                    ("user", CommandOptionKind::User, false),
                    ("taxable", CommandOptionKind::Boolean, false),
                ],
            ),
            ("player", vec![("user", CommandOptionKind::User, true)],),
            (
                "fine",
                vec![
                    ("user", CommandOptionKind::User, true),
                    ("amount", CommandOptionKind::Integer, true),
                    ("reason", CommandOptionKind::String, false),
                ],
            ),
            (
                "resetcooldown",
                vec![("user", CommandOptionKind::User, true)],
            ),
            (
                "bankruptcy",
                vec![
                    ("user", CommandOptionKind::User, true),
                    ("action", CommandOptionKind::String, true),
                    ("games", CommandOptionKind::Integer, false),
                    ("reason", CommandOptionKind::String, false),
                ],
            ),
            (
                "ledger",
                vec![
                    ("user", CommandOptionKind::User, false),
                    ("limit", CommandOptionKind::Integer, false),
                    ("page", CommandOptionKind::Integer, false),
                ],
            ),
        ]
    );
    let fine = command
        .options
        .iter()
        .find(|option| option.name == "fine")
        .expect("fine schema");
    assert_eq!(
        fine.options
            .iter()
            .map(|option| (option.name.as_str(), option.kind, option.required))
            .collect::<Vec<_>>(),
        [
            ("user", CommandOptionKind::User, true),
            ("amount", CommandOptionKind::Integer, true),
            ("reason", CommandOptionKind::String, false),
        ]
    );
    let ledger = command
        .options
        .iter()
        .find(|option| option.name == "ledger")
        .expect("ledger schema");
    assert_eq!(ledger.options[1].min_integer, Some(1));
    assert_eq!(ledger.options[1].max_integer, Some(25));
    assert_eq!(ledger.options[2].max_integer, Some(10_000));
    let bankruptcy = command
        .options
        .iter()
        .find(|option| option.name == "bankruptcy")
        .expect("bankruptcy schema");
    assert_eq!(
        bankruptcy.options[1].choices,
        [
            CommandOptionChoice::String {
                name: "add".to_owned(),
                value: "add".to_owned(),
            },
            CommandOptionChoice::String {
                name: "remove".to_owned(),
                value: "remove".to_owned(),
            },
        ]
    );
    assert_eq!(bankruptcy.options[2].min_integer, Some(0));
    assert_eq!(bankruptcy.options[2].max_integer, Some(100));
    assert_eq!(registry.component_routes()[0].custom_id_prefix, "tax:");
}

#[tokio::test]
async fn guild_and_tax_man_guards_precede_defer_and_database_reads() {
    let fixture = Fixture::migrated();
    let handler = fixture
        .registry()
        .command_handler("tax")
        .expect("tax handler");
    let dm = Arc::new(Responder::default());
    handler
        .handle(command(TAX_MAN, None, "audit", Vec::new(), 1), dm.clone())
        .await
        .expect("DM guard");
    assert_eq!(
        dm.immediate.lock().expect("immediate")[0].content,
        GUILD_ONLY_MESSAGE
    );
    assert!(dm.defers.lock().expect("defers").is_empty());

    let denied = Arc::new(Responder::default());
    handler
        .handle(
            command(123, Some(GUILD), "audit", Vec::new(), 2),
            denied.clone(),
        )
        .await
        .expect("permission guard");
    assert_eq!(
        denied.immediate.lock().expect("immediate")[0].content,
        "Only Tax Men can use this command."
    );
    assert!(denied.defers.lock().expect("defers").is_empty());

    let mut discord_admin = command(123, Some(GUILD), "audit", Vec::new(), 20);
    let InteractionRequest::Command {
        member_permissions, ..
    } = &mut discord_admin
    else {
        unreachable!("command fixture")
    };
    *member_permissions = Some(MANAGE_GUILD_PERMISSION);
    let allowed = Arc::new(Responder::default());
    handler
        .handle(discord_admin, allowed.clone())
        .await
        .expect("Discord Manage Server permission");
    assert_eq!(allowed.defers.lock().expect("defers").as_slice(), [true]);
    assert!(allowed.immediate.lock().expect("immediate").is_empty());
}

#[tokio::test]
async fn public_event_and_vanity_do_not_require_tax_man() {
    let fixture = Fixture::migrated();
    let handler = fixture
        .registry()
        .command_handler("tax")
        .expect("tax handler");
    let event = Arc::new(Responder::default());
    handler
        .handle(
            command(123, Some(GUILD), "event", Vec::new(), 3),
            event.clone(),
        )
        .await
        .expect("public event");
    assert_eq!(event.defers.lock().expect("defers").as_slice(), [false]);
    assert!(!event.followups.lock().expect("followups")[0].ephemeral);
    assert_eq!(
        event.followups.lock().expect("followups")[0].embeds[0]
            .title
            .as_deref(),
        Some("🌤️ The Economy Is Between Spells")
    );

    let now = Utc::now().timestamp();
    let planner = EconomyEventService::new(
        MemoryEventStore::default(),
        MemoryEconomy::default(),
        FixedClock::new(now),
        DeterministicEventRng,
        fixture.provider.handler.economy.config.clone(),
    );
    let event_date = planner
        .event_date_for_timestamp(now)
        .expect("current event date");
    fixture
        .connection()
        .execute(
            "INSERT INTO economy_daily_events(
                 guild_id,event_date,name,hero,direction,severity,target_effect_jc,
                 forecast_flow_jc,expected_effect_jc,monetary_stock_before,effects,
                 announcement,starts_at,ends_at,created_at
             ) VALUES (?1,?2,'Ravage','Tidehunter','deflationary',2,0,0,0,0,
                       '{}','The tide turns.',?3,?4,?3)",
            params![i64::try_from(GUILD).unwrap(), event_date, now, now + 3_600],
        )
        .expect("seed current economy event");
    let active_event = Arc::new(Responder::default());
    handler
        .handle(
            command(123, Some(GUILD), "event", Vec::new(), 30),
            active_event.clone(),
        )
        .await
        .expect("public active event");
    let active_embed = active_event.followups.lock().expect("followups")[0].embeds[0].clone();
    assert_eq!(active_embed.title.as_deref(), Some("🌑 Ravage — Level II"));
    assert_eq!(
        active_embed.thumbnail_url.as_deref(),
        Some(
            "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/abilities/tidehunter_ravage.png"
        )
    );

    let vanity = Arc::new(Responder::default());
    handler
        .handle(
            command(123, Some(GUILD), "vanity", Vec::new(), 4),
            vanity.clone(),
        )
        .await
        .expect("public vanity");
    assert!(vanity.defers.lock().expect("defers").is_empty());
    assert!(vanity.immediate.lock().expect("immediate")[0].ephemeral);
}

#[tokio::test]
async fn audit_and_policy_use_live_migrated_economy_state() {
    let fixture = Fixture::migrated();
    fixture.player(1, 25);
    let handler = fixture.registry().command_handler("tax").expect("handler");

    let audit = Arc::new(Responder::default());
    handler
        .handle(
            command(TAX_MAN, Some(GUILD), "audit", Vec::new(), 40),
            audit.clone(),
        )
        .await
        .expect("guild tax audit");
    assert_eq!(audit.defers.lock().expect("defers").as_slice(), [true]);
    let audit_response = audit.followups.lock().expect("followups")[0].clone();
    assert!(audit_response.ephemeral);
    assert_eq!(
        audit_response.embeds[0].title.as_deref(),
        Some("Tax Man Guild Audit")
    );
    assert!(
        audit_response.embeds[0].fields[0]
            .value
            .contains("Players: 1")
    );

    let policy = Arc::new(Responder::default());
    handler
        .handle(
            command(TAX_MAN, Some(GUILD), "policy", Vec::new(), 41),
            policy.clone(),
        )
        .await
        .expect("monetary policy");
    assert_eq!(policy.defers.lock().expect("defers").as_slice(), [true]);
    let policy_response = policy.followups.lock().expect("followups")[0].clone();
    assert!(policy_response.ephemeral);
    assert_eq!(
        policy_response.embeds[0].title.as_deref(),
        Some("Jopacoin Monetary Policy")
    );
    assert!(
        policy_response.embeds[0]
            .description
            .as_deref()
            .is_some_and(|description| description.contains("**Mode:** Disabled"))
    );
}

#[tokio::test]
async fn migrated_legacy_severe_event_preserves_missing_voting_flag_fallback() {
    let fixture = Fixture::migrated();
    let now = Utc::now().timestamp();
    let planner = EconomyEventService::new(
        MemoryEventStore::default(),
        MemoryEconomy::default(),
        FixedClock::new(now),
        DeterministicEventRng,
        fixture.provider.handler.economy.config.clone(),
    );
    let event_date = planner
        .event_date_for_timestamp(now)
        .expect("current event date");
    fixture
        .connection()
        .execute(
            "INSERT INTO economy_daily_events(
                 guild_id,event_date,name,hero,direction,severity,target_effect_jc,
                 forecast_flow_jc,expected_effect_jc,monetary_stock_before,effects,
                 announcement,starts_at,ends_at,created_at
             ) VALUES (?1,?2,'Global Silence','Silencer','deflationary',3,0,0,0,0,
                       '{\"reward_multiplier\":0.9}','Legacy silence.',?3,?4,?3)",
            params![i64::try_from(GUILD).unwrap(), event_date, now, now + 3_600],
        )
        .expect("seed legacy severe event");
    let responder = Arc::new(Responder::default());
    fixture
        .registry()
        .command_handler("tax")
        .expect("handler")
        .handle(
            command(123, Some(GUILD), "event", Vec::new(), 45),
            responder.clone(),
        )
        .await
        .expect("legacy public event");
    let response = responder.followups.lock().expect("followups")[0].clone();
    assert!(!response.ephemeral);
    assert_eq!(
        response.embeds[0].title.as_deref(),
        Some("🌑 Global Silence — Level III")
    );
    assert!(response.embeds[0].fields.iter().any(|field| {
        field
            .value
            .contains("Reserve allocation voting is suspended")
    }));
}

#[tokio::test]
async fn vanity_admin_override_persists_and_clears_through_live_service() {
    let fixture = Fixture::migrated_with_config(config_with_vanity_rate("0.123456"));
    let handler = fixture.registry().command_handler("tax").expect("handler");
    let override_option = |enforced| {
        vec![
            user("user", 7),
            InteractionOption {
                name: "taxable".to_owned(),
                value: InteractionValue::Boolean(enforced),
            },
        ]
    };

    let enforce = Arc::new(Responder::default());
    handler
        .handle(
            command(TAX_MAN, Some(GUILD), "vanity", override_option(true), 42),
            enforce.clone(),
        )
        .await
        .expect("enforce vanity tax");
    assert_eq!(enforce.defers.lock().expect("defers").as_slice(), [true]);
    assert!(
        enforce.followups.lock().expect("followups")[0]
            .content
            .contains("now manually subject")
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM vanity_tax_enforcements
                 WHERE guild_id=?1 AND discord_id=7",
                [i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .expect("persisted vanity override"),
        1
    );

    let status = Arc::new(Responder::default());
    handler
        .handle(
            command(123, Some(GUILD), "vanity", vec![user("user", 7)], 44),
            status.clone(),
        )
        .await
        .expect("manual vanity status");
    assert!(
        status.immediate.lock().expect("immediate")[0]
            .content
            .contains("12.3456% vanity tax")
    );

    let clear = Arc::new(Responder::default());
    handler
        .handle(
            command(TAX_MAN, Some(GUILD), "vanity", override_option(false), 43),
            clear.clone(),
        )
        .await
        .expect("clear vanity tax");
    assert!(
        clear.followups.lock().expect("followups")[0]
            .content
            .contains("returned to the automatic nickname rule")
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM vanity_tax_enforcements
                 WHERE guild_id=?1 AND discord_id=7",
                [i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .expect("cleared vanity override"),
        0
    );
}

#[tokio::test]
async fn fine_is_atomic_and_deducts_exact_amount_from_negative_balance() {
    let fixture = Fixture::migrated();
    fixture.player(1, -25);
    fixture
        .connection()
        .execute_batch("DELETE FROM economy_ledger_entries; DELETE FROM economy_ledger_context;")
        .expect("clear initial player ledger");
    let responder = Arc::new(Responder::default());
    fixture
        .registry()
        .command_handler("tax")
        .expect("handler")
        .handle(
            command(
                TAX_MAN,
                Some(GUILD),
                "fine",
                vec![
                    user("user", 1),
                    InteractionOption {
                        name: "amount".to_owned(),
                        value: InteractionValue::Integer(100),
                    },
                    InteractionOption {
                        name: "reason".to_owned(),
                        value: InteractionValue::String("audit debt".to_owned()),
                    },
                ],
                5,
            ),
            responder.clone(),
        )
        .await
        .expect("fine command");
    let response = responder.followups.lock().expect("followups")[0].clone();
    assert!(response.ephemeral);
    assert!(response.content.contains("Levied a 100"));
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=1 AND guild_id=?1",
                [i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .expect("fine balance"),
        -125
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id=?1",
                [i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .expect("fine reserve"),
        100
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries WHERE source='tax_fine'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("fine ledger count"),
        2
    );

    let invalid = Arc::new(Responder::default());
    fixture
        .registry()
        .command_handler("tax")
        .expect("handler")
        .handle(
            command(
                TAX_MAN,
                Some(GUILD),
                "fine",
                vec![
                    user("user", 404),
                    InteractionOption {
                        name: "amount".to_owned(),
                        value: InteractionValue::Integer(0),
                    },
                ],
                6,
            ),
            invalid.clone(),
        )
        .await
        .expect("invalid fine");
    assert_eq!(
        invalid.followups.lock().expect("followups")[0].content,
        "Fine amount must be positive."
    );
}

#[tokio::test]
async fn player_reports_registered_audit_and_exact_missing_target_copy() {
    let fixture = Fixture::migrated();
    fixture.player(1, -12);
    let handler = fixture.registry().command_handler("tax").expect("handler");

    let found = Arc::new(Responder::default());
    handler
        .handle(
            command(TAX_MAN, Some(GUILD), "player", vec![user("user", 1)], 500),
            found.clone(),
        )
        .await
        .expect("registered player audit");
    let found_response = found.followups.lock().expect("followups")[0].clone();
    assert!(found_response.ephemeral);
    assert_eq!(
        found_response.embeds[0].title.as_deref(),
        Some("Tax Man Player Audit - Player 1")
    );
    assert!(found_response.embeds[0].fields[0].value.contains("-12"));

    let missing = Arc::new(Responder::default());
    handler
        .handle(
            command(TAX_MAN, Some(GUILD), "player", vec![user("user", 404)], 501),
            missing.clone(),
        )
        .await
        .expect("missing player audit");
    assert_eq!(missing.defers.lock().expect("defers").as_slice(), [true]);
    let missing_response = missing.followups.lock().expect("followups")[0].clone();
    assert!(missing_response.ephemeral);
    assert_eq!(
        missing_response.content,
        "That user is not registered in this guild."
    );
    assert!(missing_response.components.is_empty());
}

#[tokio::test]
async fn resetcooldown_and_bankruptcy_mutate_the_live_database() {
    let fixture = Fixture::migrated();
    fixture.player(1, -10);
    fixture
        .connection()
        .execute(
            "INSERT INTO tax_fine_cooldowns(
                 discord_id,guild_id,last_fined_at,last_amount,last_actor_id
             ) VALUES (1,?1,123,5,99)",
            [i64::try_from(GUILD).unwrap()],
        )
        .expect("seed cooldown");
    let handler = fixture.registry().command_handler("tax").expect("handler");

    let reset = Arc::new(Responder::default());
    handler
        .handle(
            command(
                TAX_MAN,
                Some(GUILD),
                "resetcooldown",
                vec![user("user", 1)],
                510,
            ),
            reset.clone(),
        )
        .await
        .expect("reset cooldown");
    assert_eq!(reset.defers.lock().expect("defers").as_slice(), [true]);
    assert_eq!(
        reset.followups.lock().expect("followups")[0].content,
        "Reset Tax Man fine cooldown for Player 1."
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM tax_fine_cooldowns
                 WHERE discord_id=1 AND guild_id=?1",
                [i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .expect("cooldown count"),
        0
    );

    let add = Arc::new(Responder::default());
    handler
        .handle(
            command(
                TAX_MAN,
                Some(GUILD),
                "bankruptcy",
                vec![
                    user("user", 1),
                    InteractionOption {
                        name: "action".to_owned(),
                        value: InteractionValue::String("add".to_owned()),
                    },
                ],
                511,
            ),
            add.clone(),
        )
        .await
        .expect("add bankruptcy modifier");
    assert!(
        add.followups.lock().expect("followups")[0]
            .content
            .contains("Added 3 bankruptcy modifier game(s) to Player 1")
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT penalty_games_remaining FROM bankruptcy_state
                 WHERE discord_id=1 AND guild_id=?1",
                [i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .expect("bankruptcy penalty"),
        3
    );

    let remove = Arc::new(Responder::default());
    handler
        .handle(
            command(
                TAX_MAN,
                Some(GUILD),
                "bankruptcy",
                vec![
                    user("user", 1),
                    InteractionOption {
                        name: "action".to_owned(),
                        value: InteractionValue::String("remove".to_owned()),
                    },
                ],
                512,
            ),
            remove.clone(),
        )
        .await
        .expect("remove bankruptcy modifier");
    assert!(
        remove.followups.lock().expect("followups")[0]
            .content
            .contains("Removed bankruptcy modifier from Player 1")
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT penalty_games_remaining FROM bankruptcy_state
                 WHERE discord_id=1 AND guild_id=?1",
                [i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .expect("cleared bankruptcy penalty"),
        0
    );
}

#[tokio::test]
async fn ledger_pager_is_owner_scoped_and_edits_next_page() {
    let fixture = Fixture::migrated();
    fixture.player(1, 0);
    let connection = fixture.connection();
    connection
        .execute("DELETE FROM economy_ledger_entries", [])
        .expect("clear ledger");
    for index in 1..=3_i64 {
        connection
            .execute(
                "INSERT INTO economy_ledger_entries(
                     guild_id,account_type,account_id,delta,balance_before,
                     balance_after,source,created_at
                 ) VALUES (?1,'player',1,?2,0,?2,'fixture',?2)",
                params![i64::try_from(GUILD).unwrap(), index],
            )
            .expect("seed paged ledger");
    }
    let handler = fixture.registry().command_handler("tax").expect("handler");
    let initial = Arc::new(Responder::default());
    handler
        .handle(
            command(
                TAX_MAN,
                Some(GUILD),
                "ledger",
                vec![InteractionOption {
                    name: "limit".to_owned(),
                    value: InteractionValue::Integer(1),
                }],
                600,
            ),
            initial.clone(),
        )
        .await
        .expect("ledger command");
    let first = initial.followups.lock().expect("followups")[0].clone();
    assert_eq!(first.components.len(), 1);
    assert!(first.components[0].buttons[0].disabled);
    assert!(!first.components[0].buttons[1].disabled);
    let expires_before_denial = fixture
        .provider
        .handler
        .load_view(600)
        .expect("load ledger view")
        .expect("stored ledger view")
        .expires_at();
    tokio::time::sleep(Duration::from_millis(1)).await;

    let denied = Arc::new(Responder::default());
    handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 601,
                custom_id: "tax:ledger:600:next".to_owned(),
                user_id: 123,
                user_display_name: "Intruder".to_owned(),
                guild_id: Some(GUILD),
                channel_id: Some(7),
                member_permissions: None,
                values: Vec::new(),
            },
            denied.clone(),
        )
        .await
        .expect("owner denial");
    assert_eq!(
        denied.immediate.lock().expect("immediate")[0].content,
        "This ledger view belongs to another Tax Man."
    );
    let expires_after_denial = fixture
        .provider
        .handler
        .load_view(600)
        .expect("reload ledger view")
        .expect("refreshed ledger view")
        .expires_at();
    assert!(expires_after_denial > expires_before_denial);

    let next = Arc::new(Responder::default());
    handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 602,
                custom_id: "tax:ledger:600:next".to_owned(),
                user_id: TAX_MAN,
                user_display_name: "Tax Man".to_owned(),
                guild_id: Some(GUILD),
                channel_id: Some(7),
                member_permissions: None,
                values: Vec::new(),
            },
            next.clone(),
        )
        .await
        .expect("next page");
    assert_eq!(next.defers.lock().expect("defers").as_slice(), [false]);
    let edit = &next.edits.lock().expect("edits")[0];
    assert!(
        edit.embeds[0]
            .footer
            .as_deref()
            .is_some_and(|footer| footer.contains("Page 2/3"))
    );
}

#[tokio::test]
async fn expired_pager_is_removed_and_matches_discord_py_no_ack_timeout() {
    let fixture = Fixture::migrated();
    let view = TaxLedgerView::new(GUILD, TAX_MAN, None, 1, 1, 2);
    fixture
        .provider
        .handler
        .views
        .lock()
        .expect("views")
        .insert(
            700,
            StoredTaxView::Ledger {
                expires_at: Instant::now() - Duration::from_secs(1),
                view,
            },
        );
    let responder = Arc::new(Responder::default());
    fixture
        .provider
        .handler
        .handle_component("tax:ledger:700:next", TAX_MAN, responder.clone())
        .await
        .expect("expired component");
    assert!(responder.defers.lock().expect("defers").is_empty());
    assert!(responder.immediate.lock().expect("immediate").is_empty());
    assert!(responder.followups.lock().expect("followups").is_empty());
    assert!(
        fixture
            .provider
            .handler
            .load_view(700)
            .expect("load expired view")
            .is_none()
    );
}

#[test]
fn pager_custom_ids_round_trip() {
    assert!(matches!(
        parse_component_id("tax:ledger:42:next"),
        Ok(("ledger", 42, PageDirection::Next))
    ));
}
