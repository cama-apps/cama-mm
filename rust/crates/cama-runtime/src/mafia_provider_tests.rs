//! Focused tests for the private Mafia registration surface.
//!
//! This file is intentionally only reached when the parent runtime admits the
//! private provider module.  Keep the test module next to the unexported
//! implementation so promotion cannot accidentally omit its contract tests.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use cama_app::service_container::{ServiceContainer, ServiceContainerOptions};
use cama_db::low_priority_repository::SetLowPriorityInput;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use crate::discord_transport::{
    DiscordEmoji, DiscordGuildMemberSnapshot, DiscordMessage, DiscordMessageReceipt,
    DiscordMessageSnapshot, DiscordTransport,
};
use crate::gateway_events::{GatewayMember, GuildMemberPageSource, ReadyRecoveryContext};
use crate::registration::{
    InteractionHandler, InteractionResponder, InteractionResponseError, RegistryBuilder,
};
use crate::{FirstGamePoolGuildSource, WorkerContext};

use super::*;

#[test]
fn registration_tree_matches_python_mafia_surface() {
    let options = mafia_options();
    assert_eq!(options.len(), 13, "12 user leaves plus the admin group");
    let expected = [
        ("role", "Privately reveal your role in today's mafia game"),
        ("act", "Submit your nightly mafia action"),
        ("vote", "Vote to lynch a player during the day phase"),
        (
            "bounty",
            "Stake 1 jopacoin on a suspect — pays out if they're lynched and were mafia",
        ),
        ("join", "Reserve a seat in the next mafia game"),
        ("remind", "Ping players who still need to act this phase"),
        ("status", "Show today's mafia game status"),
        ("history", "Your mafia game history"),
        ("leaderboard", "Mafia leaderboard for this guild"),
        ("info", "How to play Daily Mafia"),
        (
            "optout",
            "Stop required actions and cancel your next-game signup",
        ),
        ("optin", "Resume required actions in Mafia"),
    ];
    for (option, (name, description)) in options.iter().zip(expected) {
        assert_eq!(option.kind, CommandOptionKind::Subcommand);
        assert_eq!(option.name, name);
        assert_eq!(option.description, description);
        assert!(!option.required);
    }

    let act = &options[1];
    assert_eq!(act.options.len(), 1);
    assert_eq!(act.options[0].name, "target");
    assert_eq!(
        act.options[0].description,
        "Player to act on (Bookie: who you predict the town will lynch)"
    );
    assert_eq!(act.options[0].kind, CommandOptionKind::User);
    assert!(act.options[0].required);

    let vote = &options[2];
    assert_eq!(vote.options[0].name, "target");
    assert_eq!(vote.options[0].description, "Player to vote against");
    assert_eq!(vote.options[0].kind, CommandOptionKind::User);
    assert!(vote.options[0].required);

    let bounty = &options[3];
    assert_eq!(bounty.options[0].name, "target");
    assert_eq!(
        bounty.options[0].description,
        "The player you suspect is mafia"
    );
    assert_eq!(bounty.options[0].kind, CommandOptionKind::User);
    assert!(bounty.options[0].required);

    let admin = &options[12];
    assert_eq!(admin.kind, CommandOptionKind::SubcommandGroup);
    assert_eq!(admin.name, "admin");
    assert_eq!(admin.description, "Mafia admin controls");
    assert_eq!(admin.options.len(), 3);
    assert_eq!(
        admin
            .options
            .iter()
            .map(|option| (option.name.as_str(), option.description.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("start", "Force-start a new mafia game"),
            ("stop", "End the running game now with a standing tally"),
            ("abort", "Cancel the running game and refund entry fees"),
        ]
    );
    assert!(
        admin
            .options
            .iter()
            .all(|option| option.kind == CommandOptionKind::Subcommand)
    );
}

#[test]
fn role_assignment_uses_unique_bundled_dota_names() {
    let ids = (1..=8).collect::<Vec<_>>();
    let mut rng = fastrand::Rng::with_seed(7);
    let players = assign_roles(42, &ids, &mut rng);
    let names = players
        .iter()
        .map(|player| player.hero_name.clone().expect("hero"))
        .collect::<BTreeSet<_>>();
    assert_eq!(names.len(), ids.len());
    assert!(names.iter().all(|name| !name.starts_with("Hero")));
}

struct EconomyFixture {
    database: NamedTempFile,
    repository: MafiaRepository,
}

impl EconomyFixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary migrated Mafia database");
        crate::test_support::initialize_test_database(database.path())
            .expect("initialize full migrated SQLite schema");
        Self {
            repository: MafiaRepository::new(database.path()),
            database,
        }
    }

    fn seed_player(&self, discord_id: i64, balance: i64) {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance,lowest_balance_ever)
                 VALUES (?1,?2,?3,?4,?4)",
                params![discord_id, 77_i64, format!("user_{discord_id}"), balance],
            )
            .expect("seed player wallet");
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.database.path()).expect("open economy fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("disable fixture foreign keys");
        connection
    }
}

#[tokio::test]
async fn migrated_sqlite_resolution_wires_bookie_mvp_overflow_and_taxes() {
    let fixture = EconomyFixture::new();
    for id in [1_i64, 2, 3] {
        fixture.seed_player(id, 100);
    }
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO bankruptcy_state(discord_id,guild_id,penalty_games_remaining)
             VALUES (2,77,2)",
            [],
        )
        .expect("seed bankruptcy penalty");
    drop(connection);

    let game_id = fixture
        .repository
        .create_game(Some(77), "2026-08-12", MafiaPhase::Day, 1_000, 15, None)
        .expect("create terminal test game");
    fixture
        .repository
        .add_players(
            game_id,
            &[
                MafiaPlayer::new(game_id, 1, 77, MafiaRole::Mafia),
                MafiaPlayer::new(game_id, 2, 77, MafiaRole::Townie),
                MafiaPlayer::new(game_id, 3, 77, MafiaRole::Bookie),
            ],
        )
        .expect("seed game roster");
    fixture
        .repository
        .record_action(&MafiaAction {
            game_id,
            guild_id: 77,
            actor_id: 2,
            target_id: Some(1),
            action_type: MafiaActionType::Vote,
            phase: MafiaPhase::Day,
            day_number: 1,
            result: None,
        })
        .expect("record lynch vote");
    fixture
        .repository
        .record_action(&MafiaAction {
            game_id,
            guild_id: 77,
            actor_id: 3,
            target_id: Some(1),
            action_type: MafiaActionType::Wager,
            phase: MafiaPhase::Night,
            day_number: 1,
            result: None,
        })
        .expect("record Bookie wager");

    let mut container =
        ServiceContainer::new(fixture.database.path(), ServiceContainerOptions::default());
    container.initialize();
    let vanity_tax = Arc::clone(
        &container
            .components()
            .expect("economy service components")
            .vanity_tax_service,
    );
    vanity_tax.update_member(77, 2, None);
    let discord = Arc::new(RecordingDiscord::default());
    let mafia_port = Arc::new(RecordingMafiaPort::default());
    let provider = MafiaRegistrationProvider::new_with_mafia_port(
        fixture.database.path(),
        &economy_config(),
        vanity_tax,
        discord,
        mafia_port,
    );
    provider
        .handler
        .tick_guild(77)
        .await
        .expect("resolve migrated SQLite game through provider");
    assert_eq!(
        fixture
            .repository
            .player_balance(2, Some(77))
            .expect("town wallet"),
        119
    );
    assert_eq!(
        fixture
            .repository
            .player_balance(3, Some(77))
            .expect("Bookie wallet"),
        150
    );
    assert_eq!(
        fixture
            .repository
            .nonprofit_fund(Some(77))
            .expect("reserve"),
        20
    );
    let settled = fixture
        .repository
        .get_game_by_id(game_id)
        .expect("read settled game")
        .expect("settled game");
    assert_eq!(settled.phase, MafiaPhase::Resolved);
    assert_eq!(settled.winner, Some(MafiaWinner::Town));
    let receipt = settled
        .resolution_summary
        .expect("atomic resolution receipt");
    assert!(receipt.contains("\"bookie_id\":3"));
    assert!(receipt.contains("\"bookie_payout\":50"));
    assert!(receipt.contains("\"nonprofit_overflow\":20"));
    assert!(receipt.contains("\"bankruptcy_penalties\":{\"2\":21}"));
    assert!(receipt.contains("\"vanity_taxes\":{\"2\":10}"));

    let ledger = fixture
        .connection()
        .prepare(
            "SELECT account_id,delta,source,related_type,related_id,reason,metadata
             FROM economy_ledger_entries WHERE guild_id=77 ORDER BY ledger_id",
        )
        .expect("prepare ledger query")
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })
        .expect("read ledger")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect ledger");
    assert!(
        ledger
            .iter()
            .any(|row| row.0 == 2 && row.1 == 50 && row.2 == "mafia_payout")
    );
    assert!(ledger.iter().any(|row| row.0 == 2 && row.1 == -21));
    assert!(ledger.iter().any(|row| row.0 == 2 && row.1 == -10));
    assert!(ledger.iter().any(|row| row.0 == 3 && row.1 == 50));
    assert!(ledger.iter().any(|row| row.0 == 77 && row.1 == 20));
    let payout_ledger = ledger
        .iter()
        .find(|row| row.0 == 2 && row.1 == 50 && row.2 == "mafia_payout")
        .expect("payout ledger context");
    let game_id_text = game_id.to_string();
    assert_eq!(payout_ledger.3.as_deref(), Some("mafia_game"));
    assert_eq!(payout_ledger.4.as_deref(), Some(game_id_text.as_str()));
    assert_eq!(payout_ledger.5.as_deref(), Some("mafia pot payout"));
    assert_eq!(
        payout_ledger.6.as_deref(),
        Some("{\"winner\":\"TOWN\",\"entry_fee\":8,\"payout_per_winner\":50}")
    );
    let bankruptcy_ledger = ledger
        .iter()
        .find(|row| row.0 == 2 && row.1 == -21)
        .expect("bankruptcy ledger context");
    assert_eq!(bankruptcy_ledger.2, "mafia_bankruptcy_penalty");
    assert_eq!(bankruptcy_ledger.3.as_deref(), Some("mafia_game"));
    assert_eq!(
        bankruptcy_ledger.5.as_deref(),
        Some("mafia bankruptcy penalty")
    );
    assert_eq!(
        bankruptcy_ledger.6.as_deref(),
        Some("{\"winner\":\"TOWN\",\"entry_fee\":8}")
    );
    let vanity_ledger = ledger
        .iter()
        .find(|row| row.0 == 2 && row.1 == -10)
        .expect("vanity ledger context");
    assert_eq!(vanity_ledger.2, "vanity_tax");
    assert_eq!(vanity_ledger.3.as_deref(), Some("mafia_game"));
    assert_eq!(vanity_ledger.5.as_deref(), Some("vanity tax on JC profit"));
    assert_eq!(
        vanity_ledger.6.as_deref(),
        Some("{\"winner\":\"TOWN\",\"entry_fee\":8}")
    );
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("ledger context clear"),
        0
    );
}

#[tokio::test]
async fn migrated_sqlite_resolution_taxes_active_low_priority_profit() {
    let fixture = EconomyFixture::new();
    for id in [1_i64, 2, 3] {
        fixture.seed_player(id, 100);
    }
    LowPriorityRepository::new(fixture.database.path())
        .set_low_priority(&SetLowPriorityInput::new(2, Some(77), 900, None))
        .expect("seed active low priority");

    let game_id = fixture
        .repository
        .create_game(Some(77), "2026-08-12", MafiaPhase::Day, 1_000, 15, None)
        .expect("create terminal test game");
    fixture
        .repository
        .add_players(
            game_id,
            &[
                MafiaPlayer::new(game_id, 1, 77, MafiaRole::Mafia),
                MafiaPlayer::new(game_id, 2, 77, MafiaRole::Townie),
                MafiaPlayer::new(game_id, 3, 77, MafiaRole::Townie),
            ],
        )
        .expect("seed game roster");
    fixture
        .repository
        .record_action(&MafiaAction {
            game_id,
            guild_id: 77,
            actor_id: 2,
            target_id: Some(1),
            action_type: MafiaActionType::Vote,
            phase: MafiaPhase::Day,
            day_number: 1,
            result: None,
        })
        .expect("record lynch vote");

    let mut container =
        ServiceContainer::new(fixture.database.path(), ServiceContainerOptions::default());
    container.initialize();
    let vanity_tax = Arc::clone(
        &container
            .components()
            .expect("economy service components")
            .vanity_tax_service,
    );
    let discord = Arc::new(RecordingDiscord::default());
    let mafia_port = Arc::new(RecordingMafiaPort::default());
    let provider = MafiaRegistrationProvider::new_with_mafia_port(
        fixture.database.path(),
        &economy_config(),
        vanity_tax,
        discord,
        mafia_port,
    );
    provider
        .handler
        .tick_guild(77)
        .await
        .expect("resolve migrated SQLite game through provider");

    // A 50 JC payout on an 8 JC entry fee is 42 JC of profit, taxed at 10%.
    assert_eq!(
        fixture
            .repository
            .player_balance(2, Some(77))
            .expect("low-priority town wallet"),
        146
    );
    assert_eq!(
        fixture
            .repository
            .player_balance(3, Some(77))
            .expect("untaxed town wallet"),
        150
    );
    let receipt = fixture
        .repository
        .get_game_by_id(game_id)
        .expect("read settled game")
        .expect("settled game")
        .resolution_summary
        .expect("atomic resolution receipt");
    assert!(receipt.contains("\"vanity_taxes\":{}"));
    assert!(receipt.contains("\"low_priority_taxes\":{\"2\":4}"));

    let ledger = fixture
        .connection()
        .prepare(
            "SELECT account_id,delta,source,related_type,related_id,reason,metadata
             FROM economy_ledger_entries WHERE guild_id=77 ORDER BY ledger_id",
        )
        .expect("prepare ledger query")
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })
        .expect("read ledger")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect ledger");
    assert!(!ledger.iter().any(|row| row.2 == "vanity_tax"));
    let low_priority_ledger = ledger
        .iter()
        .find(|row| row.0 == 2 && row.1 == -4)
        .expect("low priority ledger context");
    let game_id_text = game_id.to_string();
    assert_eq!(low_priority_ledger.2, "low_priority_tax");
    assert_eq!(low_priority_ledger.3.as_deref(), Some("mafia_game"));
    assert_eq!(
        low_priority_ledger.4.as_deref(),
        Some(game_id_text.as_str())
    );
    assert_eq!(
        low_priority_ledger.5.as_deref(),
        Some("low priority tax on JC profit")
    );
    assert_eq!(
        low_priority_ledger.6.as_deref(),
        Some("{\"winner\":\"TOWN\",\"entry_fee\":8}")
    );
}

#[test]
fn migrated_python_resolved_games_are_baselined_without_ready_replay() {
    let database = NamedTempFile::new().expect("historical database");
    crate::test_support::initialize_test_database(database.path())
        .expect("initialize historical schema");
    let connection = Connection::open(database.path()).expect("open historical schema");
    connection
        .execute(
            "INSERT INTO mafia_games(
                 guild_id,game_date,phase,started_at,roster_size,status,
                 resolution_message_id,resolution_summary
             ) VALUES (77,'2026-08-11','RESOLVED',1,5,'ACTIVE',NULL,NULL)",
            [],
        )
        .expect("seed Python-resolved game");
    connection
        .execute(
            "INSERT INTO mafia_games(
                 guild_id,game_date,phase,started_at,roster_size,status,
                 resolution_message_id,resolution_summary
             ) VALUES (77,'2026-08-10','RESOLVED',1,5,'ACTIVE',NULL,'{}')",
            [],
        )
        .expect("seed Rust-receipted game awaiting Discord delivery");
    drop(connection);

    // The migration marker is already present. READY must still distinguish a
    // Python-era NULL/NULL row from a Rust receipt that has exact copy but no
    // Discord message ID yet.
    cama_db::schema_manager::initialize_or_migrate(database.path())
        .expect("reopen migrated schema");
    let repository = MafiaRepository::new(database.path());
    let pending = repository
        .get_unannounced_resolved_games(Some(77))
        .expect("read pending resolution receipts");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].game_id, 2);
    assert_eq!(
        repository
            .get_game_by_id(1)
            .expect("read historical game")
            .expect("historical game")
            .resolution_message_id,
        Some(0)
    );
}

#[derive(Default)]
struct CompositionMafiaPort {
    thread_messages: std::sync::Mutex<Vec<(u64, DiscordMessage)>>,
}

#[async_trait]
impl MafiaDiscordPort for CompositionMafiaPort {
    async fn create_private_thread(
        &self,
        _channel_id: u64,
        _name: &str,
        _member_ids: &[u64],
    ) -> Result<Option<u64>, String> {
        Ok(Some(70_001))
    }

    async fn create_public_thread(
        &self,
        _channel_id: u64,
        _parent_message_id: u64,
        _name: &str,
    ) -> Result<u64, String> {
        Ok(70_002)
    }

    async fn add_private_thread_members(
        &self,
        _thread_id: u64,
        _member_ids: &[u64],
    ) -> Result<(), String> {
        Ok(())
    }

    async fn send_thread_message(
        &self,
        thread_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.thread_messages
            .lock()
            .expect("thread messages")
            .push((thread_id, message));
        Ok(())
    }

    async fn archive_thread(&self, _thread_id: u64, _name: &str) -> Result<(), String> {
        Ok(())
    }
}

struct CompositionGuilds;

impl FirstGamePoolGuildSource for CompositionGuilds {
    fn live_guild_ids(&self) -> Result<Vec<i64>, String> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
struct RecordingDiscord {
    state: std::sync::Mutex<RecordingDiscordState>,
}

#[derive(Default)]
struct RecordingDiscordState {
    next_message_id: u64,
    fail_send: bool,
    fail_next_send: bool,
    fail_edit: bool,
    fetched: usize,
    first_send_barrier: Option<Arc<tokio::sync::Barrier>>,
    sent: Vec<(u64, DiscordMessage)>,
    edited: Vec<(u64, u64, DiscordMessage)>,
    pinned: Vec<(u64, u64)>,
    unpinned: Vec<(u64, u64)>,
}

impl RecordingDiscord {
    fn set_fail_send(&self, fail: bool) {
        self.state.lock().expect("Discord state").fail_send = fail;
    }

    fn fail_next_send(&self) {
        self.state.lock().expect("Discord state").fail_next_send = true;
    }

    fn set_fail_edit(&self, fail: bool) {
        self.state.lock().expect("Discord state").fail_edit = fail;
    }

    fn set_first_send_barrier(&self, barrier: Arc<tokio::sync::Barrier>) {
        self.state.lock().expect("Discord state").first_send_barrier = Some(barrier);
    }

    fn sent_count(&self) -> usize {
        self.state.lock().expect("Discord state").sent.len()
    }

    fn edited_count(&self) -> usize {
        self.state.lock().expect("Discord state").edited.len()
    }

    fn unpinned_count(&self) -> usize {
        self.state.lock().expect("Discord state").unpinned.len()
    }

    fn fetched_count(&self) -> usize {
        self.state.lock().expect("Discord state").fetched
    }

    fn sent_messages(&self) -> Vec<(u64, DiscordMessage)> {
        self.state.lock().expect("Discord state").sent.clone()
    }
}

#[async_trait]
impl DiscordTransport for RecordingDiscord {
    async fn fetch_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
    ) -> Result<Option<DiscordMessageSnapshot>, String> {
        self.state.lock().expect("Discord state").fetched += 1;
        Ok(None)
    }

    async fn send_message(
        &self,
        channel_id: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        let barrier = self
            .state
            .lock()
            .expect("Discord state")
            .first_send_barrier
            .take();
        if let Some(barrier) = barrier {
            barrier.wait().await;
        }
        let mut state = self.state.lock().expect("Discord state");
        if state.fail_send || state.fail_next_send {
            state.fail_next_send = false;
            return Err("simulated public send failure".to_owned());
        }
        state.next_message_id += 1;
        let message_id = state.next_message_id;
        state.sent.push((channel_id, message));
        Ok(DiscordMessageReceipt {
            channel_id,
            message_id,
            jump_url: format!("https://discord.invalid/{channel_id}/{message_id}"),
        })
    }

    async fn edit_message(
        &self,
        channel_id: u64,
        message_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        let mut state = self.state.lock().expect("Discord state");
        state.edited.push((channel_id, message_id, message));
        if state.fail_edit {
            return Err("simulated standings edit failure".to_owned());
        }
        Ok(())
    }

    async fn delete_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn create_public_thread(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _name: &str,
    ) -> Result<u64, String> {
        Ok(80_001)
    }

    async fn pin_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        self.state
            .lock()
            .expect("Discord state")
            .pinned
            .push((channel_id, message_id));
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

    async fn unpin_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        self.state
            .lock()
            .expect("Discord state")
            .unpinned
            .push((channel_id, message_id));
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
struct RecordingMafiaPort {
    state: std::sync::Mutex<RecordingMafiaState>,
}

#[derive(Default)]
struct RecordingMafiaState {
    next_thread_id: u64,
    private_creates: usize,
    public_creates: usize,
    first_private_create_barrier: Option<Arc<tokio::sync::Barrier>>,
    archives: Vec<(u64, String)>,
    private_members: Vec<(u64, Vec<u64>)>,
    thread_messages: Vec<(u64, DiscordMessage)>,
    fail_thread_message: bool,
}

impl RecordingMafiaPort {
    fn private_creates(&self) -> usize {
        self.state.lock().expect("Mafia port state").private_creates
    }

    fn public_creates(&self) -> usize {
        self.state.lock().expect("Mafia port state").public_creates
    }

    fn archives(&self) -> Vec<(u64, String)> {
        self.state
            .lock()
            .expect("Mafia port state")
            .archives
            .clone()
    }

    fn set_fail_thread_message(&self, fail: bool) {
        self.state
            .lock()
            .expect("Mafia port state")
            .fail_thread_message = fail;
    }

    fn set_first_private_create_barrier(&self, barrier: Arc<tokio::sync::Barrier>) {
        self.state
            .lock()
            .expect("Mafia port state")
            .first_private_create_barrier = Some(barrier);
    }

    fn thread_message_count(&self) -> usize {
        self.state
            .lock()
            .expect("Mafia port state")
            .thread_messages
            .len()
    }

    fn private_member_batches(&self) -> Vec<(u64, Vec<u64>)> {
        self.state
            .lock()
            .expect("Mafia port state")
            .private_members
            .clone()
    }
}

#[async_trait]
impl MafiaDiscordPort for RecordingMafiaPort {
    async fn create_private_thread(
        &self,
        _channel_id: u64,
        _name: &str,
        member_ids: &[u64],
    ) -> Result<Option<u64>, String> {
        let barrier = self
            .state
            .lock()
            .expect("Mafia port state")
            .first_private_create_barrier
            .take();
        if let Some(barrier) = barrier {
            barrier.wait().await;
        }
        let mut state = self.state.lock().expect("Mafia port state");
        state.private_creates += 1;
        state.next_thread_id += 1;
        let thread_id = 81_000 + state.next_thread_id;
        state.private_members.push((thread_id, member_ids.to_vec()));
        Ok(Some(thread_id))
    }

    async fn create_public_thread(
        &self,
        _channel_id: u64,
        _parent_message_id: u64,
        _name: &str,
    ) -> Result<u64, String> {
        let mut state = self.state.lock().expect("Mafia port state");
        state.public_creates += 1;
        state.next_thread_id += 1;
        Ok(82_000 + state.next_thread_id)
    }

    async fn add_private_thread_members(
        &self,
        thread_id: u64,
        member_ids: &[u64],
    ) -> Result<(), String> {
        self.state
            .lock()
            .expect("Mafia port state")
            .private_members
            .push((thread_id, member_ids.to_vec()));
        Ok(())
    }

    async fn send_thread_message(
        &self,
        thread_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        let mut state = self.state.lock().expect("Mafia port state");
        if state.fail_thread_message {
            return Err("simulated private intro failure".to_owned());
        }
        state.thread_messages.push((thread_id, message));
        Ok(())
    }

    async fn send_thread_message_with_receipt(
        &self,
        thread_id: u64,
        message: DiscordMessage,
    ) -> Result<Option<u64>, String> {
        self.send_thread_message(thread_id, message).await?;
        Ok(Some(thread_id.saturating_add(10_000)))
    }

    async fn archive_thread(&self, thread_id: u64, name: &str) -> Result<(), String> {
        self.state
            .lock()
            .expect("Mafia port state")
            .archives
            .push((thread_id, name.to_owned()));
        Ok(())
    }

    async fn resolve_public_channel(
        &self,
        _guild_id: u64,
        configured_channel_id: u64,
    ) -> Result<Option<u64>, String> {
        Ok(Some(configured_channel_id))
    }
}

struct MediaFixture {
    _database: NamedTempFile,
    repository: MafiaRepository,
    provider: MafiaRegistrationProvider,
    discord: Arc<RecordingDiscord>,
    mafia_port: Arc<RecordingMafiaPort>,
    game: MafiaGame,
}

fn media_fixture() -> MediaFixture {
    let database = NamedTempFile::new().expect("media database");
    crate::test_support::initialize_test_database(database.path())
        .expect("initialize media schema");
    let connection = Connection::open(database.path()).expect("open media schema");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production SQLite foreign-key mode");
    for id in 1_i64..=5 {
        connection
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,jopacoin_balance,lowest_balance_ever
                 ) VALUES (?1,77,?2,100,100)",
                params![id, format!("media_{id}")],
            )
            .expect("seed media player");
    }
    drop(connection);

    let repository = MafiaRepository::new(database.path());
    let game_id = repository
        .create_game(Some(77), "2026-08-12", MafiaPhase::Night, 1_000, 5, None)
        .expect("create media game");
    Connection::open(database.path())
        .expect("reopen media schema for entry fee")
        .execute(
            "UPDATE mafia_games SET entry_fee=8 WHERE game_id=?1",
            [game_id],
        )
        .expect("seed media entry fee");
    let players = [
        (1, MafiaRole::Mafia),
        (2, MafiaRole::Mafia),
        (3, MafiaRole::Doctor),
        (4, MafiaRole::Detective),
        (5, MafiaRole::Townie),
    ]
    .into_iter()
    .map(|(discord_id, role)| {
        let mut player = MafiaPlayer::new(game_id, discord_id, 77, role);
        player.hero_name = Some(format!("MediaHero{discord_id}"));
        player
    })
    .collect::<Vec<_>>();
    repository
        .add_players(game_id, &players)
        .expect("seed media roster");
    let game = repository
        .get_game_by_id(game_id)
        .expect("read media game")
        .expect("media game");

    let mut container = ServiceContainer::new(database.path(), ServiceContainerOptions::default());
    container.initialize();
    let vanity_tax = Arc::clone(
        &container
            .components()
            .expect("media service components")
            .vanity_tax_service,
    );
    let discord = Arc::new(RecordingDiscord::default());
    let mafia_port = Arc::new(RecordingMafiaPort::default());
    let provider = MafiaRegistrationProvider::new_with_mafia_port(
        database.path(),
        &composition_config(),
        vanity_tax,
        discord.clone(),
        mafia_port.clone(),
    );
    MediaFixture {
        _database: database,
        repository,
        provider,
        discord,
        mafia_port,
        game,
    }
}

fn second_media_provider(fixture: &MediaFixture) -> MafiaRegistrationProvider {
    let mut container =
        ServiceContainer::new(fixture._database.path(), ServiceContainerOptions::default());
    container.initialize();
    let vanity_tax = Arc::clone(
        &container
            .components()
            .expect("second Mafia service components")
            .vanity_tax_service,
    );
    let discord: Arc<dyn DiscordTransport> = fixture.discord.clone();
    let mafia_port: Arc<dyn MafiaDiscordPort> = fixture.mafia_port.clone();
    MafiaRegistrationProvider::new_with_mafia_port(
        fixture._database.path(),
        &composition_config(),
        vanity_tax,
        discord,
        mafia_port,
    )
}

fn mafia_leaf(name: &str, children: Vec<InteractionOption>) -> Vec<InteractionOption> {
    vec![InteractionOption {
        name: name.to_owned(),
        value: InteractionValue::Subcommand(children),
    }]
}

fn mafia_admin_leaf(name: &str) -> Vec<InteractionOption> {
    vec![InteractionOption {
        name: "admin".to_owned(),
        value: InteractionValue::SubcommandGroup(vec![InteractionOption {
            name: name.to_owned(),
            value: InteractionValue::Subcommand(Vec::new()),
        }]),
    }]
}

fn mafia_request(actor_id: u64, options: Vec<InteractionOption>) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: actor_id,
        name: "mafia".to_owned(),
        user_id: actor_id,
        user_display_name: format!("Mafia {actor_id}"),
        guild_id: Some(77),
        channel_id: Some(crate::application_config::MAFIA_CHANNEL_ID as u64),
        member_permissions: None,
        options,
    }
}

async fn dispatch_mafia_command(
    fixture: &MediaFixture,
    actor_id: u64,
    options: Vec<InteractionOption>,
) -> Arc<CompositionResponder> {
    let responder = Arc::new(CompositionResponder::default());
    fixture
        .provider
        .handler
        .handle(mafia_request(actor_id, options), responder.clone())
        .await
        .expect("dispatch Mafia command");
    responder
}

#[tokio::test]
async fn every_mafia_command_leaf_dispatches_through_live_handler() {
    let fixture = media_fixture();
    let role = dispatch_mafia_command(&fixture, 1, mafia_leaf("role", Vec::new())).await;
    let role_response = response_of(&role);
    assert_eq!(role_response.content, "");
    assert!(role_response.ephemeral);
    assert_eq!(
        role_response.embeds[0].title.as_deref(),
        Some("🔪 You are a Mafia")
    );
    assert_eq!(role_response.embeds[0].fields[0].name, "Mafia allies");

    let target = InteractionOption {
        name: "target".to_owned(),
        value: InteractionValue::User {
            id: 5,
            display_name: Some("Mafia 5".to_owned()),
            is_bot: Some(false),
        },
    };
    let act = dispatch_mafia_command(&fixture, 1, mafia_leaf("act", vec![target.clone()])).await;
    assert_eq!(
        response_of(&act).content,
        "🔪 Kill vote registered for <@5>."
    );
    let vote = dispatch_mafia_command(&fixture, 5, mafia_leaf("vote", vec![target.clone()])).await;
    assert_eq!(response_of(&vote).content, "Day phase is not active.");
    let bounty = dispatch_mafia_command(&fixture, 5, mafia_leaf("bounty", vec![target])).await;
    assert_eq!(
        response_of(&bounty).content,
        "Bounties can only be placed during the day."
    );

    let join = dispatch_mafia_command(&fixture, 5, mafia_leaf("join", Vec::new())).await;
    assert_eq!(
        response_of(&join).content,
        "✅ You're queued for the next available Mafia roster. Seats go to registered, entry-fee-eligible players in signup order."
    );
    let remind = dispatch_mafia_command(&fixture, 5, mafia_leaf("remind", Vec::new())).await;
    assert!(
        response_of(&remind)
            .content
            .contains("Please submit your night action with `/mafia act`.")
    );
    let status = dispatch_mafia_command(&fixture, 5, mafia_leaf("status", Vec::new())).await;
    let status_response = response_of(&status);
    assert_eq!(
        status_response.embeds[0].title.as_deref(),
        Some("Cama Mafia #1 — NIGHT")
    );
    assert_eq!(status_response.embeds[0].fields[0].name, "Alive");
    let history = dispatch_mafia_command(&fixture, 5, mafia_leaf("history", Vec::new())).await;
    assert_eq!(response_of(&history).content, "No mafia history yet.");
    let leaderboard =
        dispatch_mafia_command(&fixture, 5, mafia_leaf("leaderboard", Vec::new())).await;
    assert_eq!(
        response_of(&leaderboard).content,
        "No resolved mafia games yet."
    );
    let info = dispatch_mafia_command(&fixture, 5, mafia_leaf("info", Vec::new())).await;
    let info_response = response_of(&info);
    assert_eq!(
        info_response.embeds[0].title.as_deref(),
        Some("Cama Mafia — Rules")
    );
    let info_text = mafia_info_text();
    assert_eq!(
        info_response.embeds[0].description.as_deref(),
        Some(info_text.as_str())
    );
    let optout = dispatch_mafia_command(&fixture, 5, mafia_leaf("optout", Vec::new())).await;
    assert_eq!(
        response_of(&optout).content,
        "You're opted out. Mafia won't wait for or remind you, and any next-game signup was cancelled. Use `/mafia optin` to resume."
    );
    let optin = dispatch_mafia_command(&fixture, 5, mafia_leaf("optin", Vec::new())).await;
    assert_eq!(
        response_of(&optin).content,
        "Welcome back. Use `/mafia join` if you want a seat in the next game."
    );

    let admin_fixture = media_fixture();
    let start = dispatch_mafia_command(&admin_fixture, 42, mafia_admin_leaf("start")).await;
    assert_eq!(
        response_of(&start).content,
        "✅ Started Cama Mafia #1 with 5 players."
    );
    let stop = dispatch_mafia_command(&admin_fixture, 42, mafia_admin_leaf("stop")).await;
    assert_eq!(
        response_of(&stop).content,
        "🛑 Stopped & resolved — **TOWN** wins."
    );
    let abort = dispatch_mafia_command(&admin_fixture, 42, mafia_admin_leaf("abort")).await;
    assert_eq!(response_of(&abort).content, "No active game to abort.");
}

fn response_of(responder: &Arc<CompositionResponder>) -> InteractionResponse {
    let responses = responder.responses.lock().expect("interaction responses");
    assert_eq!(
        responses.len(),
        1,
        "command should produce exactly one response"
    );
    responses[0].clone()
}

#[tokio::test]
async fn python_mafia_role_command_case() {
    let fixture = media_fixture();
    let response =
        response_of(&dispatch_mafia_command(&fixture, 1, mafia_leaf("role", Vec::new())).await);
    assert_eq!(
        response.embeds[0].title.as_deref(),
        Some("🔪 You are a Mafia")
    );
}

#[tokio::test]
async fn python_mafia_act_command_case() {
    let fixture = media_fixture();
    let target = InteractionOption {
        name: "target".to_owned(),
        value: InteractionValue::User {
            id: 5,
            display_name: Some("Mafia 5".to_owned()),
            is_bot: Some(false),
        },
    };
    assert_eq!(
        response_of(&dispatch_mafia_command(&fixture, 1, mafia_leaf("act", vec![target])).await,)
            .content,
        "🔪 Kill vote registered for <@5>."
    );
}

#[test]
fn user_option_distinguishes_absent_from_out_of_range_target() {
    assert_eq!(user_option(&[], "target"), Ok(None));
    let valid = InteractionOption {
        name: "target".to_owned(),
        value: InteractionValue::User {
            id: 5,
            display_name: None,
            is_bot: Some(false),
        },
    };
    assert_eq!(
        user_option(std::slice::from_ref(&valid), "target"),
        Ok(Some(5))
    );
    let out_of_range = InteractionOption {
        name: "target".to_owned(),
        value: InteractionValue::User {
            id: u64::MAX,
            display_name: None,
            is_bot: Some(false),
        },
    };
    assert_eq!(
        user_option(std::slice::from_ref(&out_of_range), "target"),
        Err(InteractionHandlerError::Handler(
            "target ID exceeds SQLite INTEGER".to_owned()
        ))
    );
}

#[tokio::test]
async fn mafia_act_reports_invalid_target_id_not_missing_target() {
    let fixture = media_fixture();
    let target = InteractionOption {
        name: "target".to_owned(),
        value: InteractionValue::User {
            id: u64::MAX,
            display_name: None,
            is_bot: Some(false),
        },
    };
    let responder = Arc::new(CompositionResponder::default());
    let error = fixture
        .provider
        .handler
        .handle(mafia_request(1, mafia_leaf("act", vec![target])), responder)
        .await
        .expect_err("out-of-range target must fail closed");
    assert_eq!(
        error,
        InteractionHandlerError::Handler("target ID exceeds SQLite INTEGER".to_owned())
    );
}

#[tokio::test]
async fn python_mafia_vote_command_case() {
    let fixture = media_fixture();
    let target = InteractionOption {
        name: "target".to_owned(),
        value: InteractionValue::User {
            id: 1,
            display_name: None,
            is_bot: Some(false),
        },
    };
    assert_eq!(
        response_of(&dispatch_mafia_command(&fixture, 5, mafia_leaf("vote", vec![target])).await,)
            .content,
        "Day phase is not active."
    );
}

#[tokio::test]
async fn python_mafia_bounty_command_case() {
    let fixture = media_fixture();
    let target = InteractionOption {
        name: "target".to_owned(),
        value: InteractionValue::User {
            id: 1,
            display_name: None,
            is_bot: Some(false),
        },
    };
    assert_eq!(
        response_of(
            &dispatch_mafia_command(&fixture, 5, mafia_leaf("bounty", vec![target])).await,
        )
        .content,
        "Bounties can only be placed during the day."
    );
}

#[tokio::test]
async fn python_mafia_join_command_case() {
    let fixture = media_fixture();
    assert_eq!(
        response_of(&dispatch_mafia_command(&fixture, 5, mafia_leaf("join", Vec::new())).await)
            .content,
        "✅ You're queued for the next available Mafia roster. Seats go to registered, entry-fee-eligible players in signup order."
    );
}

#[tokio::test]
async fn python_mafia_remind_command_case() {
    let fixture = media_fixture();
    assert!(
        response_of(&dispatch_mafia_command(&fixture, 5, mafia_leaf("remind", Vec::new())).await)
            .content
            .contains("Please submit your night action with `/mafia act`.")
    );
}

#[tokio::test]
async fn python_mafia_status_command_case() {
    let fixture = media_fixture();
    let response =
        response_of(&dispatch_mafia_command(&fixture, 5, mafia_leaf("status", Vec::new())).await);
    assert_eq!(
        response.embeds[0].title.as_deref(),
        Some("Cama Mafia #1 — NIGHT")
    );
    assert_eq!(response.embeds[0].fields[0].name, "Alive");
}

#[tokio::test]
async fn python_mafia_history_command_case() {
    let fixture = media_fixture();
    assert_eq!(
        response_of(&dispatch_mafia_command(&fixture, 5, mafia_leaf("history", Vec::new())).await)
            .content,
        "No mafia history yet."
    );
}

#[tokio::test]
async fn python_mafia_leaderboard_command_case() {
    let fixture = media_fixture();
    assert_eq!(
        response_of(
            &dispatch_mafia_command(&fixture, 5, mafia_leaf("leaderboard", Vec::new())).await,
        )
        .content,
        "No resolved mafia games yet."
    );
}

#[tokio::test]
async fn python_mafia_info_command_case() {
    let fixture = media_fixture();
    let response =
        response_of(&dispatch_mafia_command(&fixture, 5, mafia_leaf("info", Vec::new())).await);
    let info = mafia_info_text();
    assert_eq!(
        response.embeds[0].description.as_deref(),
        Some(info.as_str())
    );
}

#[tokio::test]
async fn python_mafia_optout_command_case() {
    let fixture = media_fixture();
    assert_eq!(
        response_of(&dispatch_mafia_command(&fixture, 5, mafia_leaf("optout", Vec::new())).await)
            .content,
        "You're opted out. Mafia won't wait for or remind you, and any next-game signup was cancelled. Use `/mafia optin` to resume."
    );
}

#[tokio::test]
async fn python_mafia_optin_command_case() {
    let fixture = media_fixture();
    assert_eq!(
        response_of(&dispatch_mafia_command(&fixture, 5, mafia_leaf("optin", Vec::new())).await)
            .content,
        "Welcome back. Use `/mafia join` if you want a seat in the next game."
    );
}

#[tokio::test]
async fn python_mafia_admin_start_command_case() {
    let fixture = media_fixture();
    assert_eq!(
        response_of(&dispatch_mafia_command(&fixture, 42, mafia_admin_leaf("start")).await).content,
        "✅ Started Cama Mafia #1 with 5 players."
    );
}

#[tokio::test]
async fn python_mafia_admin_stop_command_case() {
    let fixture = media_fixture();
    assert_eq!(
        response_of(&dispatch_mafia_command(&fixture, 42, mafia_admin_leaf("stop")).await).content,
        "🛑 Stopped & resolved — **TOWN** wins."
    );
}

#[tokio::test]
async fn python_mafia_admin_abort_command_case() {
    let fixture = media_fixture();
    assert_eq!(
        response_of(&dispatch_mafia_command(&fixture, 42, mafia_admin_leaf("abort")).await).content,
        "🚫 Game aborted and fees refunded."
    );
}

#[tokio::test]
async fn python_mafia_reminder_is_deduplicated_across_handler_instances() {
    let fixture = media_fixture();
    fixture
        .provider
        .handler
        .post_reminder(&fixture.game, 77)
        .await
        .expect("first reminder delivery");

    // A second provider instance must observe the same durable claim rather
    // than relying on an in-process reminder cache.
    let second = second_media_provider(&fixture);
    second
        .handler
        .post_reminder(&fixture.game, 77)
        .await
        .expect("deduplicated reminder delivery");
    assert_eq!(fixture.discord.sent_count(), 1);
    assert!(
        !fixture
            .repository
            .claim_phase_reminder(
                Some(77),
                fixture.game.game_id,
                fixture.game.day_number,
                fixture.game.phase,
            )
            .expect("read reminder claim")
    );
}

#[tokio::test]
async fn python_mafia_reminder_revalidates_recipients_after_claim() {
    let fixture = media_fixture();
    let connection =
        Connection::open(fixture._database.path()).expect("open reminder race database");
    connection
        .execute(
            "CREATE TRIGGER reminder_claim_marks_recipients_done
             AFTER INSERT ON mafia_phase_reminders
             BEGIN
                 UPDATE mafia_players SET role='TOWNIE';
             END",
            [],
        )
        .expect("install reminder claim race trigger");
    drop(connection);

    fixture
        .provider
        .handler
        .post_reminder(&fixture.game, 77)
        .await
        .expect("recipient-changed reminder is discarded");
    assert_eq!(fixture.discord.sent_count(), 0);
    assert!(
        fixture
            .repository
            .claim_phase_reminder(
                Some(77),
                fixture.game.game_id,
                fixture.game.day_number,
                fixture.game.phase,
            )
            .expect("read released reminder claim")
    );
}

#[tokio::test]
async fn python_mafia_reminder_releases_claim_when_phase_changes() {
    let fixture = media_fixture();
    let stale = fixture.game.clone();
    fixture
        .repository
        .set_phase(stale.game_id, MafiaPhase::Day, Some(1_100), None)
        .expect("advance phase during reminder claim race");

    fixture
        .provider
        .handler
        .post_reminder(&stale, 77)
        .await
        .expect("phase-changed reminder is discarded");
    assert_eq!(fixture.discord.sent_count(), 0);
    assert!(
        fixture
            .repository
            .claim_phase_reminder(Some(77), stale.game_id, stale.day_number, stale.phase)
            .expect("read released reminder claim")
    );
}

#[tokio::test]
async fn python_mafia_reminder_retries_after_http_failure() {
    let fixture = media_fixture();
    fixture.discord.fail_next_send();
    fixture
        .provider
        .handler
        .post_reminder(&fixture.game, 77)
        .await
        .expect("failed reminder is recoverable");
    assert_eq!(fixture.discord.sent_count(), 0);

    fixture
        .provider
        .handler
        .post_reminder(&fixture.game, 77)
        .await
        .expect("reminder retry after HTTP failure");
    assert_eq!(fixture.discord.sent_count(), 1);
}

#[tokio::test]
async fn python_mafia_join_rejects_unregistered_player() {
    let fixture = media_fixture();
    let response = dispatch_mafia_command(&fixture, 999, mafia_leaf("join", Vec::new())).await;
    let response = response_of(&response);
    assert!(response.ephemeral);
    assert_eq!(
        response.content,
        "You need to be registered before joining Mafia."
    );
}

#[tokio::test]
async fn python_mafia_inactive_status_directs_players_to_join() {
    let fixture = media_fixture();
    fixture
        .repository
        .set_phase(
            fixture.game.game_id,
            MafiaPhase::Resolved,
            None,
            Some(1_200),
        )
        .expect("resolve game before status query");
    let response = dispatch_mafia_command(&fixture, 5, mafia_leaf("status", Vec::new())).await;
    let response = response_of(&response);
    assert!(response.ephemeral);
    assert_eq!(
        response.content,
        "No Mafia game is active. Use `/mafia join` to queue for the next roster; it starts once at least 5 eligible players are queued."
    );
}

#[tokio::test]
async fn python_mafia_standings_uses_partial_message_without_fetch() {
    let fixture = media_fixture();
    fixture
        .repository
        .set_thread_ids(
            fixture.game.game_id,
            ThreadIds {
                standings_message_id: Some(44),
                ..ThreadIds::default()
            },
        )
        .expect("seed standings receipt");

    fixture
        .provider
        .handler
        .post_standings(&fixture.game)
        .await
        .expect("edit standings through partial message");
    assert_eq!(fixture.discord.fetched_count(), 0);
    assert_eq!(fixture.discord.edited_count(), 1);
    assert_eq!(fixture.discord.sent_count(), 0);
}

#[tokio::test]
async fn python_mafia_standings_reposts_when_partial_edit_fails() {
    let fixture = media_fixture();
    fixture
        .repository
        .set_thread_ids(
            fixture.game.game_id,
            ThreadIds {
                standings_message_id: Some(44),
                ..ThreadIds::default()
            },
        )
        .expect("seed standings receipt");
    fixture.discord.set_fail_edit(true);

    fixture
        .provider
        .handler
        .post_standings(&fixture.game)
        .await
        .expect("repost standings after edit failure");
    assert_eq!(fixture.discord.fetched_count(), 0);
    assert_eq!(fixture.discord.edited_count(), 1);
    assert_eq!(fixture.discord.sent_count(), 1);
    assert_eq!(fixture.discord.unpinned_count(), 0);
    let repaired = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read reposted standings")
        .expect("reposted game");
    assert_eq!(repaired.standings_message_id, Some(1));
}

#[tokio::test]
async fn python_mafia_admin_abort_unpins_partial_standings_message() {
    let fixture = media_fixture();
    fixture
        .repository
        .set_thread_ids(
            fixture.game.game_id,
            ThreadIds {
                standings_message_id: Some(44),
                ..ThreadIds::default()
            },
        )
        .expect("seed standings receipt before abort");

    let response = dispatch_mafia_command(&fixture, 42, mafia_admin_leaf("abort")).await;
    assert_eq!(
        response_of(&response).content,
        "🚫 Game aborted and fees refunded."
    );
    assert_eq!(fixture.discord.fetched_count(), 0);
    assert_eq!(fixture.discord.unpinned_count(), 1);
    assert_eq!(fixture.discord.sent_count(), 1);
}

#[tokio::test]
async fn python_mafia_setup_overlaps_public_and_private_routes() {
    let fixture = media_fixture();
    Connection::open(fixture._database.path())
        .expect("open setup database")
        .execute(
            "UPDATE mafia_players SET role='MAFIA' WHERE game_id=?1 AND discord_id IN (3,4)",
            [fixture.game.game_id],
        )
        .expect("seed four mafia members");
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    fixture.discord.set_first_send_barrier(Arc::clone(&barrier));
    fixture.mafia_port.set_first_private_create_barrier(barrier);

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        fixture.provider.handler.post_setup(&fixture.game),
    )
    .await
    .expect("public and private setup routes start together")
    .expect("setup routes complete");
    assert_eq!(fixture.discord.sent_count(), 2, "setup and standings posts");
    assert_eq!(fixture.mafia_port.private_creates(), 1);
    assert_eq!(fixture.mafia_port.thread_message_count(), 1);
    let batches = fixture.mafia_port.private_member_batches();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].1, vec![1, 2, 3, 4]);
}

#[tokio::test]
async fn python_mafia_graveyard_member_adds_are_bounded_to_four() {
    let fixture = media_fixture();
    let extras = (6_i64..=9)
        .map(|discord_id| MafiaPlayer::new(fixture.game.game_id, discord_id, 77, MafiaRole::Townie))
        .collect::<Vec<_>>();
    fixture
        .repository
        .add_players(fixture.game.game_id, &extras)
        .expect("seed larger graveyard roster");
    for discord_id in 1_i64..=9 {
        fixture
            .repository
            .set_player_alive(
                fixture.game.game_id,
                discord_id,
                false,
                Some(MafiaPhase::Night),
            )
            .expect("mark graveyard member dead");
    }
    fixture
        .repository
        .set_phase(fixture.game.game_id, MafiaPhase::Day, Some(1_100), None)
        .expect("advance graveyard game to day");
    fixture
        .repository
        .set_thread_ids(
            fixture.game.game_id,
            ThreadIds {
                setup_message_id: Some(90_001),
                standings_message_id: Some(90_002),
                graveyard_thread_id: Some(90_005),
                ..ThreadIds::default()
            },
        )
        .expect("seed graveyard thread receipt");
    let current = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read graveyard game")
        .expect("graveyard game");

    fixture
        .provider
        .handler
        .reconcile_day_surfaces(&current)
        .await
        .expect("reconcile graveyard members");
    let batches = fixture.mafia_port.private_member_batches();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].0, 90_005);
    assert_eq!(batches[0].1, (1_u64..=9).collect::<Vec<_>>());
    assert_eq!(fixture.mafia_port.thread_message_count(), 1);
    // The runtime hands the full membership batch to the Serenity adapter;
    // the adapter owns the per-member REST semaphore for both create and
    // reconcile paths. Keep the Python concurrency contract pinned to the
    // production transport rather than to this recording adapter.
    assert!(
        include_str!("serenity_transport.rs")
            .matches("Semaphore::new(4)")
            .count()
            >= 2
    );
}

#[tokio::test]
async fn admin_abort_uses_exact_ephemeral_copy_and_public_refund_count() {
    let fixture = media_fixture();
    let abort = dispatch_mafia_command(&fixture, 42, mafia_admin_leaf("abort")).await;
    assert_eq!(
        response_of(&abort).content,
        "🚫 Game aborted and fees refunded."
    );
    let public = fixture.discord.sent_messages();
    assert!(public.iter().any(|(_, message)| {
        message.response.content
            == "🚫 The current Mafia game was aborted by an admin. Entry fees were refunded to 5 players."
    }));
    assert_eq!(
        fixture
            .repository
            .get_game_by_id(fixture.game.game_id)
            .expect("read aborted game")
            .expect("aborted game")
            .phase,
        MafiaPhase::Resolved
    );
}

#[tokio::test]
async fn setup_public_and_private_branches_recover_independently() {
    let fixture = media_fixture();
    fixture.discord.set_fail_send(true);
    assert!(
        fixture
            .provider
            .handler
            .post_setup(&fixture.game)
            .await
            .is_err()
    );
    let partial = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read partial setup")
        .expect("partial game");
    assert!(partial.setup_message_id.is_none());
    assert!(partial.standings_message_id.is_none());
    assert!(partial.mafia_thread_id.is_some());
    assert_eq!(fixture.mafia_port.private_creates(), 1);

    fixture.discord.set_fail_send(false);
    fixture
        .provider
        .handler
        .post_setup(&partial)
        .await
        .expect("READY setup retry");
    let repaired = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read repaired setup")
        .expect("repaired game");
    assert!(repaired.setup_message_id.is_some());
    assert!(repaired.standings_message_id.is_some());
    assert_eq!(fixture.mafia_port.private_creates(), 1);
    assert_eq!(fixture.discord.sent_count(), 2, "setup plus standings");
}

#[tokio::test]
async fn ready_retries_private_intro_after_thread_id_receipt_crash() {
    let fixture = media_fixture();
    fixture.mafia_port.set_fail_thread_message(true);
    assert!(
        fixture
            .provider
            .handler
            .post_setup(&fixture.game)
            .await
            .is_err()
    );
    let partial = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read private-intro partial game")
        .expect("private-intro partial game");
    assert!(partial.mafia_thread_id.is_some());
    assert!(partial.mafia_intro_message_id.is_none());

    fixture.mafia_port.set_fail_thread_message(false);
    fixture
        .provider
        .handler
        .post_setup(&partial)
        .await
        .expect("retry private intro");
    let repaired = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read private-intro repaired game")
        .expect("private-intro repaired game");
    assert!(repaired.mafia_intro_message_id.is_some());
    assert_eq!(fixture.mafia_port.thread_message_count(), 1);

    fixture
        .provider
        .handler
        .post_setup(&repaired)
        .await
        .expect("idempotent private intro recovery");
    assert_eq!(fixture.mafia_port.thread_message_count(), 1);
}

#[tokio::test]
async fn automatic_reminder_uses_python_copy_and_releases_failed_claim() {
    let fixture = media_fixture();
    fixture
        .provider
        .handler
        .post_reminder(&fixture.game, 77)
        .await
        .expect("post automatic reminder");
    let messages = fixture.discord.sent_messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].1.response.content,
        "⚠️ <@1> <@2> <@3> <@4>\n4 players haven't acted yet — Night 1 ends <t:87400:R>."
    );

    // A failed delivery must release its durable claim so a later five-minute
    // pass can retry instead of silently suppressing the reminder forever.
    let retry_fixture = media_fixture();
    retry_fixture.discord.set_fail_send(true);
    retry_fixture
        .provider
        .handler
        .post_reminder(&retry_fixture.game, 77)
        .await
        .expect("failed reminder is recoverable");
    retry_fixture.discord.set_fail_send(false);
    retry_fixture
        .provider
        .handler
        .post_reminder(&retry_fixture.game, 77)
        .await
        .expect("retry reminder after released claim");
    assert_eq!(retry_fixture.discord.sent_count(), 1);
}

#[tokio::test]
async fn ready_day_reconciles_town_square_graveyard_and_standings_receipts() {
    let fixture = media_fixture();
    fixture
        .repository
        .set_phase(fixture.game.game_id, MafiaPhase::Day, Some(1_100), None)
        .expect("advance media game to day");
    fixture
        .repository
        .set_player_alive(fixture.game.game_id, 5, false, Some(MafiaPhase::Night))
        .expect("seed fallen player");
    fixture
        .repository
        .set_thread_ids(
            fixture.game.game_id,
            ThreadIds {
                setup_message_id: Some(90_001),
                standings_message_id: Some(90_002),
                ..ThreadIds::default()
            },
        )
        .expect("seed setup and standings receipts");
    let current = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read day game")
        .expect("day game");

    fixture
        .provider
        .handler
        .reconcile_day_surfaces(&current)
        .await
        .expect("reconcile day surfaces");
    let repaired = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read reconciled game")
        .expect("reconciled game");
    assert!(repaired.discussion_thread_id.is_some());
    assert!(repaired.graveyard_thread_id.is_some());
    assert!(repaired.graveyard_intro_message_id.is_some());
    assert_eq!(fixture.mafia_port.public_creates(), 1);
    assert_eq!(fixture.mafia_port.private_creates(), 1);
    assert_eq!(fixture.discord.edited_count(), 1);
}

#[tokio::test]
async fn ready_retries_graveyard_welcome_after_thread_id_receipt_crash() {
    let fixture = media_fixture();
    fixture
        .repository
        .set_phase(fixture.game.game_id, MafiaPhase::Day, Some(1_100), None)
        .expect("advance graveyard game to day");
    fixture
        .repository
        .set_player_alive(fixture.game.game_id, 5, false, Some(MafiaPhase::Night))
        .expect("seed graveyard fallen player");
    fixture
        .repository
        .set_thread_ids(
            fixture.game.game_id,
            ThreadIds {
                setup_message_id: Some(90_001),
                standings_message_id: Some(90_002),
                ..ThreadIds::default()
            },
        )
        .expect("seed graveyard parent receipts");
    let current = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read graveyard crash game")
        .expect("graveyard crash game");

    fixture.mafia_port.set_fail_thread_message(true);
    assert!(
        fixture
            .provider
            .handler
            .reconcile_day_surfaces(&current)
            .await
            .is_err()
    );
    let partial = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read graveyard partial")
        .expect("graveyard partial");
    assert!(partial.graveyard_thread_id.is_some());
    assert!(partial.graveyard_intro_message_id.is_none());

    fixture.mafia_port.set_fail_thread_message(false);
    fixture
        .provider
        .handler
        .reconcile_day_surfaces(&partial)
        .await
        .expect("retry graveyard welcome");
    let repaired = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read graveyard repaired")
        .expect("graveyard repaired");
    assert!(repaired.graveyard_intro_message_id.is_some());
    assert_eq!(fixture.mafia_port.thread_message_count(), 1);
}

#[tokio::test]
async fn resolution_archives_only_town_square_and_unpins_standings() {
    let fixture = media_fixture();
    fixture
        .repository
        .set_phase(
            fixture.game.game_id,
            MafiaPhase::Resolved,
            None,
            Some(1_200),
        )
        .expect("resolve media game");
    fixture
        .repository
        .set_thread_ids(
            fixture.game.game_id,
            ThreadIds {
                setup_message_id: Some(90_001),
                standings_message_id: Some(90_002),
                discussion_thread_id: Some(90_003),
                mafia_thread_id: Some(90_004),
                graveyard_thread_id: Some(90_005),
                ..ThreadIds::default()
            },
        )
        .expect("seed terminal Discord receipts");
    let summary = DaySummary {
        resolved: true,
        game_id: fixture.game.game_id,
        day_number: 1,
        winner: Some(MafiaWinner::Town),
        winning_ids: vec![3, 4, 5],
        pot_total: 40,
        payout_per_winner: 13,
        ..DaySummary::default()
    };
    fixture
        .provider
        .handler
        .post_resolution(&fixture.game, &summary)
        .await
        .expect("post terminal resolution");
    let repaired = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read terminal delivery")
        .expect("terminal game");
    assert!(repaired.resolution_summary.is_some());
    assert!(repaired.resolution_message_id.is_some());
    assert_eq!(fixture.mafia_port.archives().len(), 1);
    assert_eq!(fixture.mafia_port.archives()[0].0, 90_003);
    assert_eq!(fixture.discord.unpinned_count(), 1);
}

#[tokio::test]
async fn ready_replays_terminal_outbox_alongside_active_game() {
    let fixture = media_fixture();
    let pending_id = fixture
        .repository
        .create_game(Some(77), "2026-08-13", MafiaPhase::Night, 1_000, 5, None)
        .expect("create pending terminal game");
    fixture
        .repository
        .set_phase(pending_id, MafiaPhase::Resolved, None, Some(1_200))
        .expect("resolve pending terminal game");
    let pending_summary = DaySummary {
        resolved: true,
        game_id: pending_id,
        day_number: 1,
        winner: Some(MafiaWinner::Town),
        winning_ids: vec![1, 3, 5],
        pot_total: 40,
        payout_per_winner: 13,
        ..DaySummary::default()
    };
    let payload = serde_json::to_string(&ResolutionReceipt::from_summary(&pending_summary))
        .expect("serialize pending terminal receipt");
    fixture
        .repository
        .set_resolution_summary(pending_id, &payload)
        .expect("persist pending terminal receipt");

    // The active Night game still needs setup repair, but it must not hide
    // the independently pending terminal outbox from READY.
    let report = fixture.provider.handler.recover(&[77]).await;
    assert!(
        report.failures.is_empty(),
        "READY failures: {:?}",
        report.failures
    );
    assert_eq!(report.guilds_refreshed, 2);
    assert_eq!(
        fixture.discord.sent_count(),
        3,
        "resolution plus setup/standings"
    );
    assert!(
        fixture
            .repository
            .get_game_by_id(pending_id)
            .expect("read pending terminal game")
            .expect("pending terminal game")
            .resolution_message_id
            .is_some()
    );
}

#[tokio::test]
async fn terminal_delivery_failure_retries_from_exact_durable_receipt_once() {
    let fixture = media_fixture();
    fixture
        .repository
        .set_phase(
            fixture.game.game_id,
            MafiaPhase::Resolved,
            None,
            Some(1_200),
        )
        .expect("resolve terminal game");
    let summary = DaySummary {
        resolved: true,
        game_id: fixture.game.game_id,
        day_number: 1,
        winner: Some(MafiaWinner::Mafia),
        winning_ids: vec![1, 2],
        pot_total: 40,
        payout_per_winner: 20,
        bookie_id: Some(4),
        bookie_payout: 7,
        ..DaySummary::default()
    };

    fixture.discord.set_fail_send(true);
    assert!(
        fixture
            .provider
            .handler
            .post_resolution(&fixture.game, &summary)
            .await
            .is_err()
    );
    let persisted = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read failed-delivery game")
        .expect("failed-delivery game");
    assert!(persisted.resolution_summary.is_some());
    assert!(persisted.resolution_message_id.is_none());
    assert_eq!(fixture.discord.sent_count(), 0);

    fixture.discord.set_fail_send(false);
    fixture
        .provider
        .handler
        .post_resolution(&persisted, &DaySummary::default())
        .await
        .expect("retry terminal delivery");
    assert_eq!(fixture.discord.sent_count(), 1);
    let delivered = fixture
        .repository
        .get_game_by_id(fixture.game.game_id)
        .expect("read delivered game")
        .expect("delivered game");
    assert!(delivered.resolution_message_id.is_some());

    // A second READY/retry pass observes the message receipt and must not
    // duplicate the public announcement.
    fixture
        .provider
        .handler
        .post_resolution(&delivered, &DaySummary::default())
        .await
        .expect("idempotent terminal retry");
    assert_eq!(fixture.discord.sent_count(), 1);
}

struct EmptyMemberPages;

#[async_trait]
impl GuildMemberPageSource for EmptyMemberPages {
    async fn fetch_page(
        &self,
        _guild_id: u64,
        _after: Option<u64>,
        _limit: u64,
    ) -> Result<Vec<GatewayMember>, String> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
struct CompositionResponder {
    responses: std::sync::Mutex<Vec<InteractionResponse>>,
}

#[async_trait]
impl InteractionResponder for CompositionResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.responses
            .lock()
            .expect("interaction responses")
            .push(response);
        Ok(())
    }

    async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.respond(response).await
    }
}

fn composition_config() -> ApplicationConfig {
    ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some("42".to_owned()),
        _ => None,
    })
    .expect("Mafia composition configuration")
}

fn economy_config() -> ApplicationConfig {
    ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "BANKRUPTCY_PENALTY_RATE_PER_GAME" => Some("0.25".to_owned()),
        "VANITY_TAX_RATE" => Some("0.25".to_owned()),
        _ => None,
    })
    .expect("Mafia economy configuration")
}

#[tokio::test]
async fn provider_composes_registration_ready_observer_worker_and_live_info_command() {
    let database = NamedTempFile::new().expect("composition database");
    crate::test_support::initialize_test_database(database.path())
        .expect("initialize composition schema");
    let mut container = ServiceContainer::new(database.path(), ServiceContainerOptions::default());
    container.initialize();
    let vanity_tax = Arc::clone(
        &container
            .components()
            .expect("service container components")
            .vanity_tax_service,
    );
    let discord = Arc::new(crate::serenity_transport::SerenityDiscordTransport::new());
    let mafia_port = Arc::new(CompositionMafiaPort::default());
    let provider = MafiaRegistrationProvider::new_with_mafia_port(
        database.path(),
        &composition_config(),
        vanity_tax,
        discord,
        mafia_port,
    );

    // Exercise the production adapter path as well as the deterministic Mafia
    // port used below. The default adapter must preserve private-thread
    // failures rather than silently making a public thread.
    let default_provider = MafiaRegistrationProvider::new(
        database.path(),
        &composition_config(),
        Arc::clone(&provider.handler.vanity_tax),
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let mut default_builder = RegistryBuilder::default();
    default_builder
        .add_provider(&default_provider)
        .expect("register production Mafia adapter");
    assert!(default_builder.build().command_handler("mafia").is_some());

    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(&provider)
        .expect("register Mafia provider");
    let registry = builder.build();
    assert!(registry.command_handler("mafia").is_some());
    assert_eq!(registry.component_routes().len(), 1);

    let observer = provider.gateway_observer();
    assert_eq!(observer.name(), "mafia-ready-recovery");
    let report = observer
        .ready_recovery(ReadyRecoveryContext::new(
            Arc::<[u64]>::from([]),
            Arc::new(EmptyMemberPages),
        ))
        .await;
    assert_eq!(report.guilds_attempted, 0);
    assert!(report.failures.is_empty());

    let worker_spec = provider.worker_spec(Arc::new(CompositionGuilds));
    assert_eq!(worker_spec.name, "mafia_phase_loop");
    let (_shutdown, receiver) = tokio::sync::watch::channel(true);
    worker_spec
        .worker
        .run(WorkerContext::new(receiver))
        .await
        .expect("cancelled Mafia worker exits cleanly");

    let handler = registry.command_handler("mafia").expect("Mafia handler");
    let responder = Arc::new(CompositionResponder::default());
    handler
        .handle(
            InteractionRequest::Command {
                interaction_id: 1,
                name: "mafia".to_owned(),
                user_id: 42,
                user_display_name: "Admin".to_owned(),
                guild_id: Some(77),
                channel_id: Some(crate::application_config::MAFIA_CHANNEL_ID as u64),
                member_permissions: None,
                options: vec![InteractionOption {
                    name: "info".to_owned(),
                    value: InteractionValue::Subcommand(Vec::new()),
                }],
            },
            responder.clone(),
        )
        .await
        .expect("dispatch /mafia info");
    let responses = responder.responses.lock().expect("responses");
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].embeds.len(), 1);
    assert_eq!(
        responses[0].embeds[0].title.as_deref(),
        Some("Cama Mafia — Rules")
    );
    assert!(
        responses[0].embeds[0]
            .description
            .as_deref()
            .is_some_and(|text| text.contains("/mafia optout"))
    );
}
