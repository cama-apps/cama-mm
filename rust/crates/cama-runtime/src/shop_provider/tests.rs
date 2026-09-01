use super::*;
use cama_app::ai_services::{FlavorAiPort, FlavorPromptInput, build_flavor_prompt};
use cama_app::shop_commands::MANA_ITEMS;
use rusqlite::{Connection, params};
use tempfile::TempDir;

use crate::discord_transport::GuildPlayerNameDirectory;
use crate::registration::{InteractionAllowedMentions, InteractionResponseError, Registry};
use crate::test_support::initialize_test_database as initialize_or_migrate;

const GUILD: u64 = 9_001;
const BUYER: u64 = 101;
const TARGET: u64 = 202;

struct Fixture {
    _directory: TempDir,
    path: std::path::PathBuf,
    provider: ShopRegistrationProvider,
    discord: Arc<RecordingDiscord>,
}

impl Fixture {
    fn migrated() -> Self {
        let directory = tempfile::tempdir().expect("temporary Shop provider directory");
        let path = directory.path().join("cama.db");
        initialize_or_migrate(&path).expect("migrate Shop provider database");
        let discord = Arc::new(RecordingDiscord::default());
        let port: Arc<dyn ShopDiscordPort> = discord.clone();
        let provider = ShopRegistrationProvider::new(&path, &config(), port, None)
            .expect("construct Shop provider");
        Self {
            _directory: directory,
            path,
            provider,
            discord,
        }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(&self.path).expect("open Shop provider database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("production foreign-key mode");
        connection
    }

    fn player(&self, user_id: u64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players(
                   discord_id,guild_id,discord_username,jopacoin_balance,lowest_balance_ever)
                 VALUES(?1,?2,?3,?4,?4)",
                params![
                    i64::try_from(user_id).unwrap(),
                    i64::try_from(GUILD).unwrap(),
                    format!("Player {user_id}"),
                    balance
                ],
            )
            .expect("seed Shop player");
    }

    fn registry(&self) -> Registry {
        let mut builder = RegistryBuilder::default();
        builder
            .add_provider(&self.provider)
            .expect("register Shop provider");
        builder.build()
    }
}

#[derive(Default)]
struct RecordingDiscord {
    temporary: Mutex<Vec<(i64, InteractionResponse, Duration)>>,
}

struct StaticPlayerNames;

impl GuildPlayerNameResolver for StaticPlayerNames {
    fn names_for_guild(
        &self,
        guild_id: i64,
        _player_ids: &[i64],
    ) -> Result<GuildPlayerNameDirectory, String> {
        assert_eq!(guild_id, i64::try_from(GUILD).unwrap());
        Ok(GuildPlayerNameDirectory::new(Some(BTreeMap::from([
            (BUYER, "Buyer Display".to_owned()),
            (TARGET, "Server Target".to_owned()),
        ]))))
    }
}

#[derive(Default)]
struct CapturingFlavorAi {
    inputs: Mutex<Vec<FlavorPromptInput>>,
}

impl FlavorAiPort for CapturingFlavorAi {
    fn generate_flavor(&self, input: &FlavorPromptInput) -> Result<Option<String>, String> {
        self.inputs
            .lock()
            .expect("flavor inputs")
            .push(input.clone());
        Ok(Some("rendered flavor".to_owned()))
    }

    fn complete_insight(
        &self,
        _prompt: &str,
        _system_prompt: &str,
        _feature: &str,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }
}

#[async_trait]
impl ShopDiscordPort for RecordingDiscord {
    async fn shop_user_avatar_url(
        &self,
        _guild_id: i64,
        user_id: i64,
    ) -> Result<Option<String>, String> {
        Ok(Some(format!("https://example.test/{user_id}.png")))
    }

    async fn shop_send_temporary(
        &self,
        channel_id: i64,
        response: InteractionResponse,
        delete_after: Duration,
    ) -> Result<(), String> {
        self.temporary.lock().expect("temporary messages").push((
            channel_id,
            response,
            delete_after,
        ));
        Ok(())
    }
}

#[derive(Default)]
struct Responder {
    immediate: Mutex<Vec<InteractionResponse>>,
    defers: Mutex<Vec<bool>>,
    followups: Mutex<Vec<InteractionResponse>>,
    autocomplete: Mutex<Vec<Vec<CommandOptionChoice>>>,
    fail_defer: bool,
}

impl Responder {
    fn with_failed_defer() -> Self {
        Self {
            fail_defer: true,
            ..Self::default()
        }
    }
}

#[async_trait]
impl InteractionResponder for Responder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.immediate.lock().expect("immediate").push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.defers.lock().expect("defers").push(ephemeral);
        if self.fail_defer {
            Err(InteractionResponseError::new("expired interaction"))
        } else {
            Ok(())
        }
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.followups.lock().expect("followups").push(response);
        Ok(())
    }

    async fn autocomplete(
        &self,
        choices: Vec<CommandOptionChoice>,
    ) -> Result<(), InteractionResponseError> {
        self.autocomplete
            .lock()
            .expect("autocomplete")
            .push(choices);
        Ok(())
    }
}

fn config() -> ApplicationConfig {
    ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some("99".to_owned()),
        "PINGEDASH_TARGET_USER_ID" => Some("555".to_owned()),
        "PINGEDKEVIN_TARGET_USER_ID" => Some("556".to_owned()),
        "ECONOMY_EVENTS_ENABLED" | "NEON_DEGEN_ENABLED" => Some("false".to_owned()),
        _ => None,
    })
    .expect("Shop test configuration")
}

fn command(
    subcommand: &str,
    options: Vec<InteractionOption>,
    interaction_id: u64,
) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id,
        name: "shop".to_owned(),
        user_id: BUYER,
        user_display_name: "Buyer".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(77),
        member_permissions: None,
        options: vec![InteractionOption {
            name: subcommand.to_owned(),
            value: InteractionValue::Subcommand(options),
        }],
    }
}

fn autocomplete(current: &str) -> InteractionRequest {
    InteractionRequest::Autocomplete {
        interaction_id: 8_888,
        name: "shop".to_owned(),
        user_id: BUYER,
        guild_id: Some(GUILD),
        channel_id: Some(77),
        focused_option: "item".to_owned(),
        focused_value: current.to_owned(),
        options: vec![InteractionOption {
            name: "buy".to_owned(),
            value: InteractionValue::Subcommand(vec![string("item", current)]),
        }],
    }
}

fn string(name: &str, value: &str) -> InteractionOption {
    InteractionOption {
        name: name.to_owned(),
        value: InteractionValue::String(value.to_owned()),
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

fn runtime_config() -> ShopRuntimeConfig {
    ShopRuntimeConfig {
        announce_cost: 100,
        announce_target_cost: 200,
        jopa_coin_cost: 300,
        mystery_gift_cost: 400,
        double_or_nothing_cost: 500,
        double_or_nothing_cooldown: 60,
        recalibrate_cost: 600,
        recalibration_cooldown: 86_400,
        recalibration_initial_rd: 350.0,
        recalibration_initial_volatility: 0.06,
        soft_avoid_cost: 700,
        package_deal_base_cost: 800,
        package_deal_rating_divisor: 100.0,
        package_deal_games: 10,
        pingedash_cost: 900,
        pingedash_cooldown: 60,
        pingedash_target: Some(1),
        pingedkevin_cost: 1_000,
        pingedkevin_cooldown: 60,
        pingedkevin_target: Some(2),
        hostile_min_balance: 0,
        minigame_scale: 1.0,
        admin_user_ids: BTreeSet::new(),
    }
}

#[test]
fn command_schema_preserves_all_seven_python_shop_leaves() {
    let commands = shop_subcommands(&runtime_config());
    let names = commands
        .iter()
        .map(|command| command.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "buy",
            "pingedash",
            "pingedkevin",
            "avoids",
            "deals",
            "cancel-deal",
            "mana",
        ]
    );
    let buy = commands
        .iter()
        .find(|command| command.name == "buy")
        .expect("buy command");
    assert!(buy.options[0].autocomplete);
    assert_eq!(
        buy.options[1].description,
        "User to interact with (required for 'Announce + Tag', 'Soft Avoid', and 'Package Deal' options)"
    );
    let cancel = commands
        .iter()
        .find(|command| command.name == "cancel-deal")
        .expect("cancel-deal command");
    assert_eq!(cancel.options.len(), 1);
    assert_eq!(cancel.options[0].name, "target");
    assert!(cancel.options[0].required);
}

#[test]
fn process_local_rate_limit_uses_python_ceiling_for_retry_seconds() {
    let rates = Mutex::new(BTreeMap::new());
    assert_eq!(
        check_rate_limit(&rates, (1, 2), 1, Duration::from_secs(60)).unwrap(),
        None
    );
    assert_eq!(
        check_rate_limit(&rates, (1, 2), 1, Duration::from_secs(60)).unwrap(),
        Some(60)
    );
}

#[test]
fn targeted_flex_comparison_and_cherry_pick_match_python_truthiness_rules() {
    let buyer = FlexStats {
        balance: 500,
        wins: 8,
        losses: 2,
        win_rate: Some(80.0),
        rating: Some(1_700.9),
        total_bets: 12,
        net_pnl: 40,
        degen_score: Some(7),
        bankruptcies: 0,
    };
    let target = FlexStats {
        balance: 100,
        wins: 2,
        losses: 8,
        win_rate: Some(20.0),
        rating: Some(1_500.1),
        total_bets: 2,
        net_pnl: -10,
        degen_score: None,
        bankruptcies: 2,
    };
    assert_eq!(
        ShopInteractionHandler::flex_comparison(&buyer, &target).buyer_wins,
        vec![
            "400 more jopacoin",
            "80% vs 20% win rate",
            "200 higher rating",
            "50 better P&L",
            "fewer bankruptcies (0 vs 2)",
        ]
    );
    assert_eq!(
        ShopInteractionHandler::cherry_picked_flex_stats(&buyer, &target, "Target", 100),
        vec![
            "**+400** more jopacoin than Target",
            "**+200** higher rating",
            "**80%** win rate vs 20%",
            "**+50** better gambling P&L",
            "Only **0** bankruptcies vs 2",
            "**12** bets placed (more action)",
        ]
    );
}

#[test]
fn mana_schema_preserves_all_fifteen_fixed_choices() {
    let choices = mana_choices();
    let values = choices
        .iter()
        .map(|choice| match choice {
            CommandOptionChoice::String { value, .. } => value.as_str(),
            _ => panic!("mana choice must be a string"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        vec![
            "pyroclasm",
            "dynamite_cache",
            "wildfire",
            "insight",
            "mana_shield",
            "counterspell",
            "sapling",
            "regrowth",
            "overgrowth",
            "reprieve",
            "aegis",
            "sanctuary",
            "soul_harvest",
            "blood_pact",
            "dark_bargain",
        ]
    );
}

#[tokio::test]
async fn paid_ping_delivers_after_lapsed_defer_and_mentions_only_configured_target() {
    let fixture = Fixture::migrated();
    fixture.player(BUYER, 100);
    let responder = Arc::new(Responder::with_failed_defer());
    fixture
        .registry()
        .command_handler("shop")
        .expect("Shop handler")
        .handle(command("pingedash", Vec::new(), 1), responder.clone())
        .await
        .expect("paid ping delivery");
    assert_eq!(responder.defers.lock().expect("defers").as_slice(), [false]);
    let followup = responder.followups.lock().expect("followups")[0].clone();
    assert_eq!(
        followup.content,
        "<@555>\nhttps://tenor.com/view/hiash-gif-25282310"
    );
    assert_eq!(
        followup.allowed_mentions,
        InteractionAllowedMentions::Users(vec![555])
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        90
    );
    assert!(
        fixture
            .discord
            .temporary
            .lock()
            .expect("temporary")
            .is_empty()
    );
}

#[tokio::test]
async fn autocomplete_filters_catalog_without_waiting_on_recalibration_state() {
    let fixture = Fixture::migrated();
    fixture.player(BUYER, 1_000);
    fixture
        .connection()
        .execute(
            "INSERT INTO recalibration_state(
               discord_id,guild_id,last_recalibration_at,total_recalibrations)
             VALUES(?1,?2,?3,1)",
            params![
                i64::try_from(BUYER).unwrap(),
                i64::try_from(GUILD).unwrap(),
                unix_timestamp().unwrap()
            ],
        )
        .expect("seed recalibration cooldown");
    let responder = Arc::new(Responder::default());
    fixture
        .registry()
        .command_handler("shop")
        .expect("Shop handler")
        .handle(autocomplete("recal"), responder.clone())
        .await
        .expect("shop autocomplete");
    let choices = responder.autocomplete.lock().expect("autocomplete");
    assert_eq!(choices.len(), 1);
    assert_eq!(
        choices[0],
        vec![CommandOptionChoice::String {
            name: "Recalibrate (300 jopacoin)".to_owned(),
            value: "recalibrate".to_owned(),
        }]
    );
}

#[tokio::test]
async fn autocomplete_explains_shared_soft_avoid_pricing() {
    let fixture = Fixture::migrated();
    fixture.player(BUYER, 1_000);
    let responder = Arc::new(Responder::default());
    fixture
        .registry()
        .command_handler("shop")
        .expect("Shop handler")
        .handle(autocomplete("soft avoid"), responder.clone())
        .await
        .expect("shop autocomplete");
    let choices = responder.autocomplete.lock().expect("autocomplete");
    assert_eq!(choices.len(), 1);
    assert_eq!(
        choices[0],
        vec![CommandOptionChoice::String {
            name: "Soft Avoid (500 before 3 shared games; 250-1000 by win rate; lasts 10 games)"
                .to_owned(),
            value: "soft_avoid".to_owned(),
        }]
    );
}

#[tokio::test]
async fn targeted_announcement_preserves_python_stats_media_mentions_and_restart_balance() {
    let fixture = Fixture::migrated();
    fixture.player(BUYER, 500);
    fixture.player(TARGET, 100);
    fixture
        .connection()
        .execute(
            "UPDATE players SET wins=8,losses=2,glicko_rating=1700
             WHERE discord_id=?1 AND guild_id=?2",
            params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
        )
        .expect("seed buyer flex stats");
    fixture
        .connection()
        .execute(
            "UPDATE players SET wins=2,losses=8,glicko_rating=1500
             WHERE discord_id=?1 AND guild_id=?2",
            params![
                i64::try_from(TARGET).unwrap(),
                i64::try_from(GUILD).unwrap()
            ],
        )
        .expect("seed target flex stats");
    let responder = Arc::new(Responder::default());
    fixture
        .registry()
        .command_handler("shop")
        .expect("Shop handler")
        .handle(
            command(
                "buy",
                vec![string("item", "announce_target"), user("target", TARGET)],
                3,
            ),
            responder.clone(),
        )
        .await
        .expect("targeted announcement");
    assert_eq!(responder.defers.lock().expect("defers").as_slice(), [false]);
    let followup = responder.followups.lock().expect("followups")[0].clone();
    assert_eq!(followup.content, format!("<@{TARGET}>"));
    assert_eq!(
        followup.allowed_mentions,
        InteractionAllowedMentions::Users(vec![TARGET])
    );
    let embed = &followup.embeds[0];
    assert_eq!(embed.title.as_deref(), Some("WEALTH ANNOUNCEMENT"));
    assert_eq!(
        embed.thumbnail_url.as_deref(),
        Some(BOUNTY_HUNTER_IMAGE_URL)
    );
    assert_eq!(embed.fields[0].name, "The Numbers Don't Lie");
    assert!(embed.fields[0].value.contains("**+200** higher rating"));
    assert!(embed.fields[0].value.contains("**80%** win rate vs 20%"));
    let expected = 500 - fixture.provider.handler.config.announce_target_cost;
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        expected
    );
    let port: Arc<dyn ShopDiscordPort> = fixture.discord.clone();
    let restarted = ShopRegistrationProvider::new(&fixture.path, &config(), port, None)
        .expect("restart Shop provider on the same database");
    assert_eq!(
        restarted
            .handler
            .repository
            .player(i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap())
            .unwrap()
            .expect("buyer after restart")
            .balance,
        expected
    );
}

#[tokio::test]
async fn failed_mana_effect_refunds_debit_releases_daily_slot_and_keeps_public_copy() {
    let fixture = Fixture::migrated();
    fixture.player(BUYER, 100);
    let today =
        pacific_mana_day(unix_timestamp().expect("current timestamp")).expect("Pacific mana day");
    fixture
        .connection()
        .execute(
            "INSERT INTO player_mana(
               discord_id,guild_id,current_land,assigned_date,consumed_today,
               white_shield_remaining)
             VALUES(?1,?2,'Island',?3,0,0)",
            params![
                i64::try_from(BUYER).unwrap(),
                i64::try_from(GUILD).unwrap(),
                today
            ],
        )
        .expect("seed Island mana");
    let responder = Arc::new(Responder::default());
    fixture
        .registry()
        .command_handler("shop")
        .expect("Shop handler")
        .handle(
            command(
                "mana",
                vec![string("item", "insight"), user("target", 404)],
                2,
            ),
            responder.clone(),
        )
        .await
        .expect("compensated Insight failure");
    assert_eq!(responder.defers.lock().expect("defers").as_slice(), [false]);
    let followup = responder.followups.lock().expect("followups")[0].clone();
    assert_eq!(followup.content, "<@404> is not registered.");
    assert!(!followup.ephemeral);
    assert_eq!(
        followup.allowed_mentions,
        InteractionAllowedMentions::Users(vec![404])
    );
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        100
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT status FROM manashop_purchases WHERE purchase_id='shop-mana:9001:2'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "refunded"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM manashop_daily_uses WHERE discord_id=?1",
                [i64::try_from(BUYER).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn blood_pact_purchase_copy_advertises_an_uncapped_skim() {
    let fixture = Fixture::migrated();
    fixture.player(BUYER, 10_000);
    fixture.player(TARGET, 1_000);
    let now = unix_timestamp().expect("current timestamp");
    let today = pacific_mana_day(now).expect("Pacific mana day");
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO player_mana(
               discord_id,guild_id,current_land,assigned_date,consumed_today,
               white_shield_remaining)
             VALUES(?1,?2,?3,?4,0,0)",
            params![
                i64::try_from(BUYER).unwrap(),
                i64::try_from(GUILD).unwrap(),
                land_for_color("Black"),
                today
            ],
        )
        .expect("seed Swamp mana");
    drop(connection);

    let responder = Arc::new(Responder::default());
    fixture
        .registry()
        .command_handler("shop")
        .expect("Shop handler")
        .handle(
            command(
                "mana",
                vec![string("item", "blood_pact"), user("target", TARGET)],
                7_001,
            ),
            responder.clone(),
        )
        .await
        .expect("Blood Pact purchase");

    let followup = responder.followups.lock().expect("followups")[0].clone();
    assert!(
        followup.content.contains("with no cap"),
        "Blood Pact copy must state the skim is uncapped: {}",
        followup.content
    );
    // The 150 JC ceiling was retired; advertising it again would be a lie.
    assert!(
        !followup.content.contains("150"),
        "retired 150 JC ceiling must not reappear: {}",
        followup.content
    );
}

#[tokio::test]
async fn all_fifteen_mana_items_complete_durably_and_reject_replay_after_restart() {
    for (index, spec) in MANA_ITEMS.iter().copied().enumerate() {
        let fixture = Fixture::migrated();
        fixture.player(BUYER, 10_000);
        fixture.player(TARGET, 1_000);
        fixture.player(303, 1_000);
        let now = unix_timestamp().expect("current timestamp");
        let today = pacific_mana_day(now).expect("Pacific mana day");
        let land = land_for_color(spec.color);
        let connection = fixture.connection();
        connection
            .execute(
                "INSERT INTO player_mana(
                   discord_id,guild_id,current_land,assigned_date,consumed_today,
                   white_shield_remaining)
                 VALUES(?1,?2,?3,?4,0,0)",
                params![
                    i64::try_from(BUYER).unwrap(),
                    i64::try_from(GUILD).unwrap(),
                    land,
                    today
                ],
            )
            .expect("seed mana assignment");
        for user_id in [BUYER, TARGET] {
            connection
                .execute(
                    "INSERT INTO tunnels(
                       discord_id,guild_id,depth,max_depth,total_digs,total_jc_earned,
                       last_dig_at,luminosity,prestige_level)
                     VALUES(?1,?2,42,42,5,100,?3,88,2)",
                    params![
                        i64::try_from(user_id).unwrap(),
                        i64::try_from(GUILD).unwrap(),
                        now
                    ],
                )
                .expect("seed mana-item tunnel");
        }
        drop(connection);
        let interaction_id = u64::try_from(index).unwrap() + 10_000;
        let mut options = vec![string("item", spec.key)];
        if spec.target_required {
            options.push(user("target", TARGET));
        }
        let responder = Arc::new(Responder::default());
        fixture
            .registry()
            .command_handler("shop")
            .expect("Shop handler")
            .handle(
                command("mana", options.clone(), interaction_id),
                responder.clone(),
            )
            .await
            .unwrap_or_else(|error| panic!("{} command failed: {error}", spec.key));
        assert_eq!(
            responder.defers.lock().expect("defers").as_slice(),
            [false],
            "{} must defer publicly after durable settlement",
            spec.key
        );
        assert_eq!(
            responder.followups.lock().expect("followups").len(),
            1,
            "{} must deliver exactly one result",
            spec.key
        );
        let purchase_id = format!("shop-mana:{GUILD}:{interaction_id}");
        let connection = fixture.connection();
        assert_eq!(
            connection
                .query_row(
                    "SELECT status FROM manashop_purchases WHERE purchase_id=?1",
                    [&purchase_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "completed",
            "{} must be restart-durable",
            spec.key
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT consumed_today FROM player_mana
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            i64::from(spec.tier == ManaTier::Ultimate),
            "{} mana tap mismatch",
            spec.key
        );
        drop(connection);

        let port: Arc<dyn ShopDiscordPort> = fixture.discord.clone();
        let restarted = ShopRegistrationProvider::new(&fixture.path, &config(), port, None)
            .expect("restart Shop provider");
        let mut builder = RegistryBuilder::default();
        builder
            .add_provider(&restarted)
            .expect("register restarted Shop provider");
        let restarted_registry = builder.build();
        let retry = Arc::new(Responder::default());
        restarted_registry
            .command_handler("shop")
            .expect("restarted Shop handler")
            .handle(
                command("mana", options, interaction_id + 1_000),
                retry.clone(),
            )
            .await
            .unwrap_or_else(|error| panic!("{} restart retry failed: {error}", spec.key));
        let immediate = retry.immediate.lock().expect("immediate");
        assert_eq!(immediate.len(), 1, "{} restart retry response", spec.key);
        assert!(
            immediate[0].ephemeral,
            "{} retry must stay private",
            spec.key
        );
        assert!(
            immediate[0].content.contains("once-per-day")
                || immediate[0].content.contains("mana is spent"),
            "{} retry copy was {:?}",
            spec.key,
            immediate[0].content
        );
    }
}

#[tokio::test]
async fn all_simple_products_debit_once_deliver_publicly_and_survive_restart() {
    let fixture = Fixture::migrated();
    let starting_balance = 1_000_000;
    fixture.player(BUYER, starting_balance);
    let products = [
        (
            "announce",
            fixture.provider.handler.config.announce_cost,
            "WEALTH ANNOUNCEMENT",
        ),
        (
            "jopa_coin",
            fixture.provider.handler.config.jopa_coin_cost,
            "🪙 Jopa Coin(TM) Minted!",
        ),
        (
            "mystery_gift",
            fixture.provider.handler.config.mystery_gift_cost,
            "🎁 Mystery Gift Redeemed!",
        ),
    ];

    for (index, (item, _, expected_title)) in products.iter().enumerate() {
        let responder = Arc::new(Responder::default());
        fixture
            .registry()
            .command_handler("shop")
            .expect("Shop handler")
            .handle(
                command(
                    "buy",
                    vec![string("item", item)],
                    20_000 + u64::try_from(index).unwrap(),
                ),
                responder.clone(),
            )
            .await
            .unwrap_or_else(|error| panic!("{item} command failed: {error}"));
        assert_eq!(
            responder.defers.lock().expect("defers").as_slice(),
            [false],
            "{item} must defer publicly only after its debit"
        );
        let followups = responder.followups.lock().expect("followups");
        assert_eq!(followups.len(), 1, "{item} delivery count");
        assert!(!followups[0].ephemeral, "{item} is a public product");
        assert_eq!(
            followups[0].embeds[0].title.as_deref(),
            Some(*expected_title),
            "{item} embed"
        );
    }

    let expected_balance = starting_balance - products.iter().map(|(_, cost, _)| cost).sum::<i64>();
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        expected_balance
    );
    let port: Arc<dyn ShopDiscordPort> = fixture.discord.clone();
    let restarted = ShopRegistrationProvider::new(&fixture.path, &config(), port, None)
        .expect("restart Shop provider");
    assert_eq!(
        restarted
            .handler
            .repository
            .player(i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap())
            .unwrap()
            .expect("buyer after restart")
            .balance,
        expected_balance
    );
}

#[tokio::test]
async fn double_or_nothing_settles_once_and_restart_enforces_cooldown() {
    let fixture = Fixture::migrated();
    fixture.player(BUYER, 10_000);
    let responder = Arc::new(Responder::default());
    fixture
        .registry()
        .command_handler("shop")
        .expect("Shop handler")
        .handle(
            command("buy", vec![string("item", "double_or_nothing")], 21_000),
            responder.clone(),
        )
        .await
        .expect("Double or Nothing settlement");
    assert_eq!(responder.defers.lock().expect("defers").as_slice(), [false]);
    {
        let followups = responder.followups.lock().expect("followups");
        assert_eq!(followups.len(), 1);
        let title = followups[0].embeds[0]
            .title
            .as_deref()
            .expect("Double or Nothing title");
        assert!(title.starts_with("Double or Nothing:"));
        let description = followups[0].embeds[0]
            .description
            .as_deref()
            .expect("Double or Nothing description");
        let fallback_pool = if title.ends_with("DOUBLE!") {
            DOUBLE_OR_NOTHING_WIN_MESSAGES
        } else {
            DOUBLE_OR_NOTHING_LOSE_MESSAGES
        };
        assert!(
            fallback_pool
                .iter()
                .any(|message| description.contains(message)),
            "no-AI result must use Python's fallback pool: {description:?}"
        );
    }

    let settled_balance = fixture
        .connection()
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM double_or_nothing_spins
                 WHERE discord_id=?1 AND guild_id=?2",
                params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );

    let port: Arc<dyn ShopDiscordPort> = fixture.discord.clone();
    let restarted = ShopRegistrationProvider::new(&fixture.path, &config(), port, None)
        .expect("restart Shop provider");
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(&restarted)
        .expect("register restarted Shop provider");
    let retry = Arc::new(Responder::default());
    builder
        .build()
        .command_handler("shop")
        .expect("restarted Shop handler")
        .handle(
            command("buy", vec![string("item", "double_or_nothing")], 21_001),
            retry.clone(),
        )
        .await
        .expect("cooldown retry");
    let immediate = retry.immediate.lock().expect("immediate");
    assert_eq!(immediate.len(), 1);
    assert!(immediate[0].ephemeral);
    assert!(immediate[0].content.contains("cooldown"));
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        settled_balance
    );
}

#[tokio::test]
async fn avoid_and_package_purchases_are_private_listable_and_restart_durable() {
    let fixture = Fixture::migrated();
    fixture.player(BUYER, 100_000);
    fixture.player(TARGET, 100_000);

    for (index, item) in ["soft_avoid", "package_deal"].iter().enumerate() {
        let responder = Arc::new(Responder::default());
        fixture
            .registry()
            .command_handler("shop")
            .expect("Shop handler")
            .handle(
                command(
                    "buy",
                    vec![string("item", item), user("target", TARGET)],
                    22_000 + u64::try_from(index).unwrap(),
                ),
                responder.clone(),
            )
            .await
            .unwrap_or_else(|error| panic!("{item} command failed: {error}"));
        assert_eq!(
            responder.defers.lock().expect("defers").as_slice(),
            [true],
            "{item} acknowledgement must stay private"
        );
        let followups = responder.followups.lock().expect("followups");
        assert_eq!(followups.len(), 1);
        assert!(followups[0].ephemeral);
        assert_eq!(
            followups[0].allowed_mentions,
            InteractionAllowedMentions::None
        );
    }

    let port: Arc<dyn ShopDiscordPort> = fixture.discord.clone();
    let restarted = ShopRegistrationProvider::new(&fixture.path, &config(), port, None)
        .expect("restart Shop provider");
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(&restarted)
        .expect("register restarted Shop provider");
    let registry = builder.build();
    for (subcommand, title) in [
        ("avoids", "Your Active Soft Avoids"),
        ("deals", "Your Active Package Deals"),
    ] {
        let responder = Arc::new(Responder::default());
        registry
            .command_handler("shop")
            .expect("restarted Shop handler")
            .handle(command(subcommand, Vec::new(), 22_100), responder.clone())
            .await
            .unwrap_or_else(|error| panic!("{subcommand} command failed: {error}"));
        let immediate = responder.immediate.lock().expect("immediate");
        assert_eq!(immediate.len(), 1);
        assert!(immediate[0].ephemeral);
        assert_eq!(immediate[0].embeds[0].title.as_deref(), Some(title));
        assert!(
            immediate[0].embeds[0]
                .description
                .as_deref()
                .is_some_and(|description| description.contains(&format!("<@{TARGET}>")))
        );
    }
}

#[tokio::test]
async fn soft_avoid_renders_server_nickname_instead_of_interaction_display_name() {
    let fixture = Fixture::migrated();
    fixture.player(BUYER, 100_000);
    fixture.player(TARGET, 100_000);
    let port: Arc<dyn ShopDiscordPort> = fixture.discord.clone();
    let provider = ShopRegistrationProvider::new(&fixture.path, &config(), port, None)
        .expect("construct Shop provider")
        .with_player_names(Arc::new(StaticPlayerNames));
    let projected = provider
        .handler
        .with_rendered_player_names(CommandContext {
            interaction_id: 22_499,
            user_id: i64::try_from(BUYER).unwrap(),
            user_display_name: "Global Buyer".to_owned(),
            guild_id: i64::try_from(GUILD).unwrap(),
            channel_id: None,
            member_permissions: None,
            subcommand: "buy".to_owned(),
            options: vec![InteractionOption {
                name: "target".to_owned(),
                value: InteractionValue::User {
                    id: TARGET,
                    display_name: Some("Global Target".to_owned()),
                    is_bot: Some(false),
                },
            }],
            rendered_user_names: BTreeMap::new(),
        })
        .await
        .expect("project player names");
    let target = user_option(&projected, "target")
        .expect("parse target")
        .expect("target option");
    assert_eq!(projected.user_display_name, "Buyer Display");
    assert_eq!(target.display_name, "Server Target");
    assert_eq!(target.ai_display_name, "Server Target");
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(&provider)
        .expect("register Shop provider");
    let responder = Arc::new(Responder::default());
    builder
        .build()
        .command_handler("shop")
        .expect("Shop handler")
        .handle(
            command(
                "buy",
                vec![
                    string("item", "soft_avoid"),
                    InteractionOption {
                        name: "target".to_owned(),
                        value: InteractionValue::User {
                            id: TARGET,
                            display_name: Some("Global Target".to_owned()),
                            is_bot: Some(false),
                        },
                    },
                ],
                22_500,
            ),
            responder.clone(),
        )
        .await
        .expect("soft avoid purchase");

    let followups = responder.followups.lock().expect("followups");
    let description = followups[0].embeds[0]
        .description
        .as_deref()
        .expect("soft avoid description");
    assert!(description.contains("Server Target"));
    assert!(!description.contains("Global Target"));
}

#[test]
fn shop_ai_prompt_uses_rendered_name_instead_of_stored_username() {
    let fixture = Fixture::migrated();
    fixture.player(BUYER, 100_000);
    let ai = Arc::new(CapturingFlavorAi::default());
    let player_context: Arc<dyn PlayerContextPort> = Arc::new(ProductionShopPlayerContext::new(
        &fixture.path,
        Arc::new(StaticPlayerNames),
    ));
    let service = FlavorTextService::new(ai.clone(), player_context);

    assert_eq!(
        service.generate_event_flavor(
            Some(i64::try_from(GUILD).unwrap()),
            FlavorEvent::DoubleOrNothingWin,
            i64::try_from(BUYER).unwrap(),
            BTreeMap::from([("amount".to_owned(), AiValue::Integer(500))]),
        ),
        Some("rendered flavor".to_owned())
    );

    let inputs = ai.inputs.lock().expect("flavor inputs");
    let (system, prompt, _) = build_flavor_prompt(&inputs[0]);
    let rendered_prompt = format!("{system}\n{prompt}");
    assert!(
        rendered_prompt.contains("Buyer Display"),
        "{rendered_prompt}"
    );
    assert!(!rendered_prompt.contains("Player 101"), "{rendered_prompt}");
}

#[tokio::test]
async fn unregistered_target_without_cached_member_uses_payload_display_name() {
    let fixture = Fixture::migrated();
    fixture.player(BUYER, 100_000);
    let projected = fixture
        .provider
        .handler
        .with_rendered_player_names(CommandContext {
            interaction_id: 22_501,
            user_id: i64::try_from(BUYER).unwrap(),
            user_display_name: "Buyer Account".to_owned(),
            guild_id: i64::try_from(GUILD).unwrap(),
            channel_id: None,
            member_permissions: None,
            subcommand: "buy".to_owned(),
            options: vec![InteractionOption {
                name: "target".to_owned(),
                value: InteractionValue::User {
                    id: TARGET,
                    display_name: Some("Target Account".to_owned()),
                    is_bot: Some(false),
                },
            }],
            rendered_user_names: BTreeMap::new(),
        })
        .await
        .expect("project player names");

    let target = user_option(&projected, "target")
        .expect("parse target")
        .expect("target option");
    assert_eq!(target.display_name, "Target Account");
}

#[test]
fn arbitrary_shop_player_names_prefer_server_nicknames() {
    let fixture = Fixture::migrated();
    let provider = fixture
        .provider
        .with_player_names(Arc::new(StaticPlayerNames));

    let names = provider.handler.project_stored_player_names(
        i64::try_from(GUILD).unwrap(),
        BTreeMap::from([(i64::try_from(TARGET).unwrap(), "Stored Target".to_owned())]),
    );

    assert_eq!(
        names
            .get(&i64::try_from(TARGET).unwrap())
            .map(String::as_str),
        Some("Server Target")
    );
}

#[tokio::test]
async fn cancel_package_deal_is_private_and_removes_eligible_deal_without_refund() {
    let fixture = Fixture::migrated();
    fixture.player(BUYER, 1_000);
    fixture.player(TARGET, 1_000);
    fixture
        .connection()
        .execute(
            "UPDATE players SET last_match_date='2000-01-01 00:00:00'
             WHERE discord_id=?1 AND guild_id=?2",
            params![
                i64::try_from(TARGET).unwrap(),
                i64::try_from(GUILD).unwrap()
            ],
        )
        .expect("seed inactive target");
    let repository = PackageDealRepository::new(&fixture.path);
    let deal = repository
        .create_or_extend_deal(
            Some(i64::try_from(GUILD).unwrap()),
            i64::try_from(BUYER).unwrap(),
            i64::try_from(TARGET).unwrap(),
            7,
            1,
        )
        .expect("seed package deal");
    fixture
        .connection()
        .execute(
            "UPDATE package_deals SET created_at=1,updated_at=1 WHERE id=?1",
            params![deal.id],
        )
        .expect("age package deal");

    let responder = Arc::new(Responder::default());
    fixture
        .registry()
        .command_handler("shop")
        .expect("Shop handler")
        .handle(
            command("cancel-deal", vec![user("target", TARGET)], 22_200),
            responder.clone(),
        )
        .await
        .expect("cancel package deal");

    assert_eq!(responder.defers.lock().expect("defers").as_slice(), [true]);
    let followups = responder.followups.lock().expect("followups");
    assert_eq!(followups.len(), 1);
    assert!(followups[0].ephemeral);
    assert!(followups[0].content.contains("No jopacoin were refunded"));
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM package_deals WHERE id=?1",
                params![deal.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1_000
    );
}

#[tokio::test]
async fn cancel_package_deal_ineligible_explains_both_thirty_day_guards() {
    let fixture = Fixture::migrated();
    fixture.player(BUYER, 1_000);
    fixture.player(TARGET, 1_000);
    fixture
        .connection()
        .execute(
            "UPDATE players SET last_match_date='2000-01-01 00:00:00'
             WHERE discord_id=?1 AND guild_id=?2",
            params![
                i64::try_from(TARGET).unwrap(),
                i64::try_from(GUILD).unwrap()
            ],
        )
        .expect("seed inactive target");
    let repository = PackageDealRepository::new(&fixture.path);
    let deal = repository
        .create_or_extend_deal(
            Some(i64::try_from(GUILD).unwrap()),
            i64::try_from(BUYER).unwrap(),
            i64::try_from(TARGET).unwrap(),
            7,
            1,
        )
        .expect("seed recent package deal");

    let responder = Arc::new(Responder::default());
    fixture
        .registry()
        .command_handler("shop")
        .expect("Shop handler")
        .handle(
            command("cancel-deal", vec![user("target", TARGET)], 22_201),
            responder.clone(),
        )
        .await
        .expect("reject package deal cancellation");

    assert_eq!(responder.defers.lock().expect("defers").as_slice(), [true]);
    let followups = responder.followups.lock().expect("followups");
    assert_eq!(followups.len(), 1);
    assert!(followups[0].ephemeral);
    assert!(
        followups[0]
            .content
            .contains("target has not played for 30 days")
    );
    assert!(
        followups[0]
            .content
            .contains("deal has not been updated for 30 days")
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM package_deals WHERE id=?1",
                params![deal.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn cancel_package_deal_aborts_before_mutation_when_private_defer_fails() {
    let fixture = Fixture::migrated();
    fixture.player(TARGET, 1_000);
    fixture
        .connection()
        .execute(
            "UPDATE players SET last_match_date='2000-01-01 00:00:00'
             WHERE discord_id=?1 AND guild_id=?2",
            params![
                i64::try_from(TARGET).unwrap(),
                i64::try_from(GUILD).unwrap()
            ],
        )
        .unwrap();
    let deal = PackageDealRepository::new(&fixture.path)
        .create_or_extend_deal(
            Some(i64::try_from(GUILD).unwrap()),
            i64::try_from(BUYER).unwrap(),
            i64::try_from(TARGET).unwrap(),
            7,
            1,
        )
        .unwrap();
    fixture
        .connection()
        .execute(
            "UPDATE package_deals SET created_at=1,updated_at=1 WHERE id=?1",
            params![deal.id],
        )
        .unwrap();
    let responder = Arc::new(Responder::with_failed_defer());

    let result = fixture
        .registry()
        .command_handler("shop")
        .unwrap()
        .handle(
            command("cancel-deal", vec![user("target", TARGET)], 22_202),
            responder.clone(),
        )
        .await;

    assert!(result.is_err());
    assert_eq!(responder.defers.lock().expect("defers").as_slice(), [true]);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM package_deals WHERE id=?1",
                params![deal.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn cancel_package_deal_repository_failure_gets_private_followup() {
    let fixture = Fixture::migrated();
    fixture
        .connection()
        .execute(
            "ALTER TABLE package_deals RENAME TO unavailable_package_deals",
            [],
        )
        .unwrap();
    let responder = Arc::new(Responder::default());

    fixture
        .registry()
        .command_handler("shop")
        .unwrap()
        .handle(
            command("cancel-deal", vec![user("target", TARGET)], 22_203),
            responder.clone(),
        )
        .await
        .expect("controlled repository failure");

    assert_eq!(responder.defers.lock().expect("defers").as_slice(), [true]);
    let followups = responder.followups.lock().expect("followups");
    assert_eq!(followups.len(), 1);
    assert!(followups[0].ephemeral);
    assert!(followups[0].content.contains("could not be completed"));
}

#[tokio::test]
async fn cancel_package_deal_has_one_per_ten_seconds_cooldown() {
    let fixture = Fixture::migrated();
    fixture.player(TARGET, 1_000);
    fixture
        .connection()
        .execute(
            "UPDATE players SET last_match_date='2000-01-01 00:00:00'
             WHERE discord_id=?1 AND guild_id=?2",
            params![
                i64::try_from(TARGET).unwrap(),
                i64::try_from(GUILD).unwrap()
            ],
        )
        .unwrap();
    let deal = PackageDealRepository::new(&fixture.path)
        .create_or_extend_deal(
            Some(i64::try_from(GUILD).unwrap()),
            i64::try_from(BUYER).unwrap(),
            i64::try_from(TARGET).unwrap(),
            7,
            1,
        )
        .unwrap();
    fixture
        .connection()
        .execute(
            "UPDATE package_deals SET created_at=1,updated_at=1 WHERE id=?1",
            params![deal.id],
        )
        .unwrap();
    let registry = fixture.registry();
    let handler = registry.command_handler("shop").unwrap();
    handler
        .handle(
            command("cancel-deal", vec![user("target", TARGET)], 22_204),
            Arc::new(Responder::default()),
        )
        .await
        .unwrap();
    let responder = Arc::new(Responder::default());

    handler
        .handle(
            command("cancel-deal", vec![user("target", TARGET)], 22_205),
            responder.clone(),
        )
        .await
        .expect("cooldown response");

    let immediate = responder.immediate.lock().expect("immediate");
    assert_eq!(immediate.len(), 1);
    assert!(immediate[0].ephemeral);
    assert!(immediate[0].content.contains("cooldown"));
    assert!(responder.defers.lock().expect("defers").is_empty());
}

#[tokio::test]
async fn recalibration_debit_and_uncertainty_reset_are_atomic_and_restart_durable() {
    let fixture = Fixture::migrated();
    let starting_balance = 100_000;
    fixture.player(BUYER, starting_balance);
    fixture
        .connection()
        .execute(
            "UPDATE players
             SET wins=3,losses=2,glicko_rating=1700,glicko_rd=100,
                 glicko_volatility=0.04,os_mu=25,os_sigma=8
             WHERE discord_id=?1 AND guild_id=?2",
            params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
        )
        .expect("seed recalibration rating");
    let responder = Arc::new(Responder::default());
    fixture
        .registry()
        .command_handler("shop")
        .expect("Shop handler")
        .handle(
            command("buy", vec![string("item", "recalibrate")], 23_000),
            responder.clone(),
        )
        .await
        .expect("recalibration purchase");
    assert_eq!(responder.defers.lock().expect("defers").as_slice(), [false]);
    {
        let followups = responder.followups.lock().expect("followups");
        assert_eq!(followups.len(), 1);
        assert_eq!(
            followups[0].embeds[0].title.as_deref(),
            Some("Rating Recalibration")
        );
    }

    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT total_recalibrations FROM recalibration_state
                 WHERE discord_id=?1 AND guild_id=?2",
                params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    let (balance, rd): (i64, f64) = connection
        .query_row(
            "SELECT jopacoin_balance,glicko_rd FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        balance,
        starting_balance - fixture.provider.handler.config.recalibrate_cost
    );
    assert_eq!(rd, fixture.provider.handler.config.recalibration_initial_rd);
    drop(connection);

    let port: Arc<dyn ShopDiscordPort> = fixture.discord.clone();
    let restarted = ShopRegistrationProvider::new(&fixture.path, &config(), port, None)
        .expect("restart Shop provider");
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(&restarted)
        .expect("register restarted Shop provider");
    let retry = Arc::new(Responder::default());
    builder
        .build()
        .command_handler("shop")
        .expect("restarted Shop handler")
        .handle(
            command("buy", vec![string("item", "recalibrate")], 23_001),
            retry.clone(),
        )
        .await
        .expect("recalibration cooldown retry");
    let immediate = retry.immediate.lock().expect("immediate");
    assert_eq!(immediate.len(), 1);
    assert!(immediate[0].ephemeral);
    assert!(immediate[0].content.contains("cooldown"));
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![i64::try_from(BUYER).unwrap(), i64::try_from(GUILD).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        balance
    );
}
