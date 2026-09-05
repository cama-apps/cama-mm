use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cama_app::service_container::{ServiceContainer, ServiceContainerOptions};
use cama_domain::bankruptcy::BankruptcyPenaltyPolicy;
use rusqlite::{Connection, params};
use tempfile::TempDir;

use super::*;
use crate::discord_transport::{
    DiscordEmoji, DiscordGuildMemberSnapshot, DiscordMessageReceipt, DiscordMessageSnapshot,
};
use crate::gateway_events::{GatewayMember, GuildMemberPageSource};
use crate::registration::{InteractionResponseError, Registry};
use crate::test_support::initialize_test_database as initialize_or_migrate;

const GUILD: i64 = 9_001;
const CHANNEL: i64 = 9_002;
const CHALLENGER: i64 = 41;
const RECIPIENT: i64 = 42;

struct Fixture {
    _directory: TempDir,
    path: PathBuf,
}

impl Fixture {
    fn migrated() -> Self {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let path = directory.path().join("cama.db");
        initialize_or_migrate(&path).expect("migrate temporary database");
        let fixture = Self {
            _directory: directory,
            path,
        };
        fixture.register(CHALLENGER, 3_000, 1_500.0);
        fixture.register(RECIPIENT, 3_000, 1_600.0);
        fixture
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(&self.path).expect("open temporary database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match production SQLite foreign-key mode");
        connection
    }

    fn register(&self, user_id: i64, balance: i64, rating: f64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance,
                     glicko_rating, glicko_rd
                 ) VALUES (?1, ?2, 'duel-provider-fixture', ?3, ?4, 100)",
                params![user_id, GUILD, balance, rating],
            )
            .expect("insert duel player");
    }

    fn balance(&self, user_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![user_id, GUILD],
                |row| row.get(0),
            )
            .expect("read player balance")
    }

    fn challenge(&self) -> DuelChallenge {
        DuelChallengeRepository::new(&self.path)
            .list_pending_all()
            .expect("list pending duels")
            .into_iter()
            .next()
            .expect("pending duel")
    }

    fn challenge_by_id(&self, challenge_id: i64) -> DuelChallenge {
        DuelChallengeRepository::new(&self.path)
            .get_challenge(challenge_id, Some(GUILD))
            .expect("read challenge")
            .expect("challenge exists")
    }
}

#[derive(Default)]
struct MockDiscord {
    sends: Mutex<Vec<(u64, DiscordMessage)>>,
    edits: Mutex<Vec<(u64, u64, DiscordMessage)>>,
    deletes: Mutex<Vec<(u64, u64)>>,
    fail_sends: Mutex<bool>,
    next_message_id: Mutex<u64>,
}

impl MockDiscord {
    fn edits(&self) -> Vec<(u64, u64, DiscordMessage)> {
        self.edits.lock().expect("edits").clone()
    }

    fn sends(&self) -> Vec<(u64, DiscordMessage)> {
        self.sends.lock().expect("sends").clone()
    }
}

#[async_trait]
impl DiscordTransport for MockDiscord {
    async fn fetch_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
    ) -> Result<Option<DiscordMessageSnapshot>, String> {
        Ok(None)
    }

    async fn send_message(
        &self,
        channel_id: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        self.sends
            .lock()
            .expect("sends")
            .push((channel_id, message));
        if *self.fail_sends.lock().expect("fail sends") {
            return Err("Discord send failed".to_owned());
        }
        let mut next = self.next_message_id.lock().expect("next message id");
        if *next == 0 {
            *next = 700;
        }
        let message_id = *next;
        *next += 1;
        Ok(DiscordMessageReceipt {
            channel_id,
            message_id,
            jump_url: format!("https://discord.test/{channel_id}/{message_id}"),
        })
    }

    async fn edit_message(
        &self,
        channel_id: u64,
        message_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.edits
            .lock()
            .expect("edits")
            .push((channel_id, message_id, message));
        Ok(())
    }

    async fn delete_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        self.deletes
            .lock()
            .expect("deletes")
            .push((channel_id, message_id));
        Ok(())
    }

    async fn create_public_thread(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _name: &str,
    ) -> Result<u64, String> {
        Ok(1)
    }

    async fn pin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn archive_thread(
        &self,
        _thread_id: u64,
        _name: &str,
        _locked: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn add_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &DiscordEmoji,
        _user_id: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn clear_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn unpin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn send_direct_message(
        &self,
        _user_id: u64,
        _message: DiscordMessage,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn guild_member(
        &self,
        _guild_id: u64,
        _user_id: u64,
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
        Ok(None)
    }
}

#[derive(Default)]
struct CapturingResponder {
    responses: Mutex<Vec<InteractionResponse>>,
    defers: Mutex<Vec<bool>>,
    followups: Mutex<Vec<InteractionResponse>>,
    receipt: Mutex<Option<InteractionMessageReceipt>>,
}

impl CapturingResponder {
    fn with_receipt(message_id: u64) -> Self {
        Self {
            receipt: Mutex::new(Some(InteractionMessageReceipt {
                message_id,
                channel_id: CHANNEL as u64,
                delivery: crate::registration::InteractionMessageDelivery::InteractionFollowup,
            })),
            ..Self::default()
        }
    }
}

#[async_trait]
impl InteractionResponder for CapturingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.responses.lock().expect("responses").push(response);
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

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        self.followups.lock().expect("followups").push(response);
        Ok(*self.receipt.lock().expect("receipt"))
    }
}

struct UnusedMembers;

#[async_trait]
impl GuildMemberPageSource for UnusedMembers {
    async fn fetch_page(
        &self,
        _guild_id: u64,
        _after: Option<u64>,
        _limit: u64,
    ) -> Result<Vec<GatewayMember>, String> {
        panic!("duel recovery does not load member pages")
    }
}

fn provider(path: &Path, discord: Arc<MockDiscord>) -> DuelRegistrationProvider {
    provider_with_vanity_tax(path, discord, persistent_vanity_tax(path))
}

fn provider_with_vanity_tax(
    path: &Path,
    discord: Arc<MockDiscord>,
    vanity_tax: Arc<PersistentVanityTaxService>,
) -> DuelRegistrationProvider {
    DuelRegistrationProvider {
        handler: Arc::new(DuelInteractionHandler {
            repository: DuelChallengeRepository::new(path),
            evolution: PetEvolutionRepository::new(path),
            flavor: Arc::new(ProductionDuelFlavorRuntime::new(path, None, false)),
            profit_deductions: ProfitDeductionSource {
                database_path: path.to_path_buf(),
                bankruptcy: BankruptcyPenaltyPolicy {
                    rate_per_game: 0.05,
                },
                vanity_tax_rate: 0.10,
                low_priority_tax_rate: 0.10,
                vanity: Some(vanity_tax),
            },
            discord,
            admin_user_ids: BTreeSet::from([CHALLENGER]),
        }),
    }
}

fn persistent_vanity_tax(path: &Path) -> Arc<PersistentVanityTaxService> {
    let mut container = ServiceContainer::new(
        path,
        ServiceContainerOptions {
            vanity_tax_rate: 0.10,
            ..ServiceContainerOptions::default()
        },
    );
    container.initialize();
    Arc::clone(
        &container
            .components()
            .expect("vanity-tax service components")
            .vanity_tax_service,
    )
}

fn registry(provider: &DuelRegistrationProvider) -> Registry {
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(provider)
        .expect("register duel provider");
    builder.build()
}

fn issue_request() -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "duel".to_owned(),
        user_id: CHALLENGER as u64,
        user_display_name: "Challenger".to_owned(),
        guild_id: Some(GUILD as u64),
        channel_id: Some(CHANNEL as u64),
        member_permissions: None,
        options: vec![InteractionOption {
            name: "issue".to_owned(),
            value: InteractionValue::Subcommand(vec![
                InteractionOption {
                    name: "player".to_owned(),
                    value: InteractionValue::User {
                        id: RECIPIENT as u64,
                        display_name: Some("Recipient".to_owned()),
                        is_bot: Some(false),
                    },
                },
                InteractionOption {
                    name: "wager".to_owned(),
                    value: InteractionValue::Integer(500),
                },
            ]),
        }],
    }
}

fn resolve_request(challenge_id: i64, outcome: &str) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 3,
        name: "duel".to_owned(),
        user_id: CHALLENGER as u64,
        user_display_name: "Challenger".to_owned(),
        guild_id: Some(GUILD as u64),
        channel_id: Some(CHANNEL as u64),
        member_permissions: None,
        options: vec![InteractionOption {
            name: "resolve".to_owned(),
            value: InteractionValue::Subcommand(vec![
                InteractionOption {
                    name: "challenge_id".to_owned(),
                    value: InteractionValue::Integer(challenge_id),
                },
                InteractionOption {
                    name: "outcome".to_owned(),
                    value: InteractionValue::String(outcome.to_owned()),
                },
            ]),
        }],
    }
}

fn component_request(challenge_id: i64) -> InteractionRequest {
    InteractionRequest::Component {
        interaction_id: 2,
        custom_id: format!("duel:{challenge_id}:trial_by_combat"),
        user_id: RECIPIENT as u64,
        user_display_name: "Recipient".to_owned(),
        guild_id: Some(GUILD as u64),
        channel_id: Some(CHANNEL as u64),
        member_permissions: None,
        values: Vec::new(),
    }
}

#[test]
fn production_schema_registers_exact_duel_surface() {
    let fixture = Fixture::migrated();
    let provider = provider(&fixture.path, Arc::new(MockDiscord::default()));
    let registry = registry(&provider);
    let command = registry
        .commands()
        .find(|command| command.name == "duel")
        .expect("duel command");
    assert_eq!(command.description, "Challenges of honor");
    assert_eq!(
        command
            .options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        ["issue", "respond", "list", "resolve"]
    );
    assert_eq!(registry.component_routes().len(), 1);
    assert_eq!(registry.component_routes()[0].custom_id_prefix, "duel:");
}

#[tokio::test]
async fn issue_bind_restart_recovery_and_persistent_response_use_migrated_database() {
    let fixture = Fixture::migrated();
    let discord = Arc::new(MockDiscord::default());
    let first_provider = provider(&fixture.path, Arc::clone(&discord));
    let first_registry = registry(&first_provider);
    let issue_responder = Arc::new(CapturingResponder::with_receipt(300));
    first_registry
        .command_handler("duel")
        .expect("duel handler")
        .handle(issue_request(), issue_responder)
        .await
        .expect("issue duel");

    let challenge = fixture.challenge();
    assert_eq!(challenge.message_id, Some(300));
    assert_eq!(fixture.balance(CHALLENGER), 2_450);
    assert_eq!(fixture.balance(RECIPIENT), 3_000);

    let restarted = provider(&fixture.path, Arc::clone(&discord));
    let recovery = restarted
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            vec![GUILD as u64],
            Arc::new(UnusedMembers),
        ))
        .await;
    assert!(recovery.failures.is_empty());
    assert_eq!(recovery.guilds_refreshed, 1);
    let recovery_edit = discord.edits().last().expect("recovery edit").clone();
    assert_eq!(recovery_edit.0, CHANNEL as u64);
    assert_eq!(recovery_edit.1, 300);
    assert_eq!(
        recovery_edit.2.content_mode,
        crate::discord_transport::DiscordMessageContentMode::PreserveBody
    );
    assert!(recovery_edit.2.response.embeds.is_empty());
    assert_eq!(recovery_edit.2.response.components[0].buttons.len(), 3);

    let restarted_registry = registry(&restarted);
    let response = Arc::new(CapturingResponder::default());
    restarted_registry
        .component_handler(&format!("duel:{}:trial_by_combat", challenge.challenge_id))
        .expect("persistent duel component")
        .handle(component_request(challenge.challenge_id), response)
        .await
        .expect("accept duel after restart");
    let accepted = fixture.challenge_by_id(challenge.challenge_id);
    assert_eq!(accepted.status, DuelStatus::Accepted);
    assert_eq!(accepted.trial_type, Some(DuelTrial::TrialByCombat));
    assert_eq!(fixture.balance(RECIPIENT), 2_500);
    let edits = discord.edits();
    let terminal_edit = edits.last().expect("accepted edit");
    assert!(terminal_edit.2.response.components.is_empty());
    assert_eq!(
        terminal_edit.2.content_mode,
        crate::discord_transport::DiscordMessageContentMode::Preserve
    );
}

#[tokio::test]
async fn resolve_withholds_bankruptcy_penalty_and_vanity_tax_from_the_wager() {
    let fixture = Fixture::migrated();
    let discord = Arc::new(MockDiscord::default());
    let vanity_tax = persistent_vanity_tax(&fixture.path);
    vanity_tax
        .set_manual_taxation(GUILD, RECIPIENT, true, CHALLENGER)
        .expect("tax the recipient");
    fixture
        .connection()
        .execute(
            "INSERT INTO bankruptcy_state (
                 discord_id, guild_id, penalty_games_remaining, bankruptcy_count
             ) VALUES (?1, ?2, 1, 1)",
            params![RECIPIENT, GUILD],
        )
        .expect("seed recipient penalty");
    let provider = provider_with_vanity_tax(&fixture.path, Arc::clone(&discord), vanity_tax);
    let registry = registry(&provider);
    registry
        .command_handler("duel")
        .expect("duel handler")
        .handle(
            issue_request(),
            Arc::new(CapturingResponder::with_receipt(300)),
        )
        .await
        .expect("issue duel");
    let challenge = fixture.challenge();
    registry
        .component_handler(&format!("duel:{}:trial_by_combat", challenge.challenge_id))
        .expect("duel component")
        .handle(
            component_request(challenge.challenge_id),
            Arc::new(CapturingResponder::default()),
        )
        .await
        .expect("accept duel");
    assert_eq!(fixture.balance(RECIPIENT), 2_500);

    let responder = Arc::new(CapturingResponder::default());
    let responder_port: Arc<dyn InteractionResponder> = Arc::clone(&responder) as Arc<_>;
    registry
        .command_handler("duel")
        .expect("duel handler")
        .handle(
            resolve_request(challenge.challenge_id, "recipient_victory"),
            responder_port,
        )
        .await
        .expect("resolve duel");

    // Profit is the 500 JC wager: 5% of it for one outstanding penalty game
    // at a 5% per-game rate (25), plus 10% vanity tax (50).
    assert_eq!(fixture.balance(RECIPIENT), 2_500 + 1_000 - 25 - 50);
    assert_eq!(fixture.balance(CHALLENGER), 2_450);
    let resolved = fixture.challenge_by_id(challenge.challenge_id);
    assert_eq!(resolved.status, DuelStatus::Resolved);
    let followups = responder.followups.lock().expect("followups");
    let announcement = &followups.last().expect("resolution announcement").content;
    assert!(
        announcement.ends_with(&format!(
            "Challenge #{} is resolved: <@{RECIPIENT}> wins 925 JC (\u{2212}25 JC bankruptcy penalty, \u{2212}50 JC vanity tax).",
            challenge.challenge_id
        )),
        "{announcement}"
    );
    let ledger: Vec<(String, i64)> = fixture
        .connection()
        .prepare(
            "SELECT source, delta FROM economy_ledger_entries
             WHERE account_id = ?1 AND delta < 0 AND related_type = 'duel_challenge'
             ORDER BY ledger_id",
        )
        .expect("prepare ledger query")
        .query_map(params![RECIPIENT], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query ledger")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect ledger");
    assert_eq!(
        ledger,
        vec![
            ("duel_challenge".to_owned(), -500),
            ("duel_challenge".to_owned(), -25),
            ("vanity_tax".to_owned(), -50),
        ]
    );
}

#[tokio::test]
async fn missing_initial_receipt_atomically_refunds_and_marks_delivery_failed() {
    let fixture = Fixture::migrated();
    let provider = provider(&fixture.path, Arc::new(MockDiscord::default()));
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .command_handler("duel")
        .expect("duel handler")
        .handle(issue_request(), responder)
        .await
        .expect("failed delivery is handled");
    let stored: (String, i64) = fixture
        .connection()
        .query_row(
            "SELECT status, message_id IS NULL FROM duel_challenges LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read failed challenge");
    assert_eq!(stored, ("delivery_failed".to_owned(), 1));
    assert_eq!(fixture.balance(CHALLENGER), 3_000);
    assert_eq!(fixture.balance(RECIPIENT), 3_000);
}

#[tokio::test]
async fn ready_recovery_reposts_and_binds_an_interrupted_unbound_issue() {
    let fixture = Fixture::migrated();
    let repository = DuelChallengeRepository::new(&fixture.path);
    let challenge = repository
        .create_challenge_atomic(
            Some(GUILD),
            CHANNEL,
            CHALLENGER,
            RECIPIENT,
            500,
            unix_now(),
            2_592_000,
            7_776_000,
            86_400,
            CHALLENGER,
        )
        .expect("create interrupted challenge");
    assert_eq!(challenge.message_id, None);
    let discord = Arc::new(MockDiscord::default());
    let provider = provider(&fixture.path, Arc::clone(&discord));
    let report = provider
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            vec![GUILD as u64],
            Arc::new(UnusedMembers),
        ))
        .await;
    assert!(report.failures.is_empty());
    assert_eq!(discord.sends().len(), 1);
    let rebound = fixture.challenge_by_id(challenge.challenge_id);
    assert_eq!(rebound.message_id, Some(700));
    let edits = discord.edits();
    let edit = edits.last().expect("replacement controls");
    assert_eq!(edit.1, 700);
    assert_eq!(
        edit.2.content_mode,
        crate::discord_transport::DiscordMessageContentMode::PreserveBody
    );
}
