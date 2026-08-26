use std::sync::Mutex;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::discord_transport::GuildPlayerNameDirectory;
use crate::registration::{InteractionResponseError, Registry};
use crate::test_support::initialize_test_database as initialize_or_migrate;

const GUILD: i64 = 42;

struct FixedPlayerNames(BTreeMap<u64, String>);

impl GuildPlayerNameResolver for FixedPlayerNames {
    fn names_for_guild(
        &self,
        _guild_id: i64,
        _player_ids: &[i64],
    ) -> Result<GuildPlayerNameDirectory, String> {
        Ok(GuildPlayerNameDirectory::new(Some(self.0.clone())))
    }
}

#[derive(Default)]
struct Responder {
    immediate: Mutex<Vec<InteractionResponse>>,
    defers: Mutex<Vec<bool>>,
    followups: Mutex<Vec<InteractionResponse>>,
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
}

fn database() -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary matchup database");
    initialize_or_migrate(database.path()).expect("migrate matchup database");
    database
}

fn register(connection: &Connection, id: i64, name: &str) {
    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username) VALUES (?1,?2,?3)",
            params![id, GUILD, name],
        )
        .expect("insert player");
}

fn registry(provider: &AdvancedStatsRegistrationProvider) -> Registry {
    let mut builder = RegistryBuilder::default();
    builder.add_provider(provider).expect("register matchup");
    builder.build()
}

fn request(first: (u64, &str), second: (u64, &str), guild: Option<u64>) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "matchup".to_owned(),
        user_id: 99,
        user_display_name: "Requester".to_owned(),
        guild_id: guild,
        channel_id: Some(9),
        member_permissions: None,
        options: vec![
            InteractionOption {
                name: "player1".to_owned(),
                value: InteractionValue::User {
                    id: first.0,
                    display_name: Some(first.1.to_owned()),
                    is_bot: Some(false),
                },
            },
            InteractionOption {
                name: "player2".to_owned(),
                value: InteractionValue::User {
                    id: second.0,
                    display_name: Some(second.1.to_owned()),
                    is_bot: Some(false),
                },
            },
        ],
    }
}

#[test]
fn schema_matches_python_matchup_command() {
    let database = database();
    let provider = AdvancedStatsRegistrationProvider::new(database.path());
    let registry = registry(&provider);
    let command = registry.commands().next().expect("matchup command");
    assert_eq!(command.name, "matchup");
    assert_eq!(
        command.description,
        "View head-to-head stats between two players"
    );
    assert_eq!(
        command
            .options
            .iter()
            .map(|option| (option.name.as_str(), option.kind, option.required))
            .collect::<Vec<_>>(),
        [
            ("player1", CommandOptionKind::User, true),
            ("player2", CommandOptionKind::User, true),
        ]
    );
}

#[tokio::test]
async fn validation_preserves_dm_self_and_unregistered_visibility() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open matchup database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    register(&connection, 1, "Alice");
    let provider = AdvancedStatsRegistrationProvider::new(database.path());
    let handler = registry(&provider)
        .command_handler("matchup")
        .expect("handler");

    let dm = Arc::new(Responder::default());
    handler
        .handle(request((1, "Alice"), (2, "Bob"), None), dm.clone())
        .await
        .expect("DM response");
    assert_eq!(
        dm.immediate.lock().expect("immediate")[0].content,
        "This command can only be used in a server."
    );
    assert!(dm.immediate.lock().expect("immediate")[0].ephemeral);

    let same = Arc::new(Responder::default());
    handler
        .handle(
            request((1, "Alice"), (1, "Alice"), Some(GUILD as u64)),
            same.clone(),
        )
        .await
        .expect("self response");
    assert_eq!(same.defers.lock().expect("defers").as_slice(), [false]);
    assert!(same.followups.lock().expect("followups")[0].ephemeral);

    let missing = Arc::new(Responder::default());
    handler
        .handle(
            request((1, "Alice"), (2, "Bob"), Some(GUILD as u64)),
            missing.clone(),
        )
        .await
        .expect("missing response");
    assert_eq!(
        missing.followups.lock().expect("followups")[0].content,
        "2 is not registered."
    );
}

#[tokio::test]
async fn test_self_comparison_rejected() {
    let database = database();
    let responder = Arc::new(Responder::default());
    registry(&AdvancedStatsRegistrationProvider::new(database.path()))
        .command_handler("matchup")
        .expect("handler")
        .handle(
            request((1, "Alice"), (1, "Alice"), Some(GUILD as u64)),
            responder.clone(),
        )
        .await
        .expect("self comparison response");
    assert_eq!(
        responder.followups.lock().expect("followups")[0].content,
        "Cannot compare a player with themselves!"
    );
}

#[tokio::test]
async fn test_unregistered_player1() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open matchup database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    register(&connection, 2, "Bob");
    let responder = Arc::new(Responder::default());
    registry(&AdvancedStatsRegistrationProvider::new(database.path()))
        .command_handler("matchup")
        .expect("handler")
        .handle(
            request((1, "Alice"), (2, "Bob"), Some(GUILD as u64)),
            responder.clone(),
        )
        .await
        .expect("missing player response");
    assert_eq!(
        responder.followups.lock().expect("followups")[0].content,
        "1 is not registered."
    );
}

#[tokio::test]
async fn test_unregistered_player2() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open matchup database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    register(&connection, 1, "Alice");
    let responder = Arc::new(Responder::default());
    registry(&AdvancedStatsRegistrationProvider::new(database.path()))
        .command_handler("matchup")
        .expect("handler")
        .handle(
            request((1, "Alice"), (2, "Bob"), Some(GUILD as u64)),
            responder.clone(),
        )
        .await
        .expect("missing player response");
    assert_eq!(
        responder.followups.lock().expect("followups")[0].content,
        "2 is not registered."
    );
}

async fn assert_migrated_pairing_renders_canonical_wins_in_either_argument_order() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open matchup database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    register(&connection, 1, "Alice");
    register(&connection, 2, "Bob");
    connection
        .execute(
            "INSERT INTO player_pairings(
                 guild_id,player1_id,player2_id,games_together,wins_together,
                 games_against,player1_wins_against,last_match_id
             ) VALUES (?1,1,2,4,3,5,2,99)",
            [GUILD],
        )
        .expect("insert pairing");
    let provider = AdvancedStatsRegistrationProvider::new(database.path());
    let handler = registry(&provider)
        .command_handler("matchup")
        .expect("handler");

    for (first, second, expected_first, expected_second) in [
        ((1, "Alice"), (2, "Bob"), "1: 2 wins", "2: 3 wins"),
        ((2, "Bob"), (1, "Alice"), "2: 3 wins", "1: 2 wins"),
    ] {
        let responder = Arc::new(Responder::default());
        handler
            .handle(
                request(first, second, Some(GUILD as u64)),
                responder.clone(),
            )
            .await
            .expect("matchup response");
        let followups = responder.followups.lock().expect("followups");
        let embed = &followups[0].embeds[0];
        assert_eq!(embed.fields[0].value, "4 games, 3 wins (75%)");
        assert!(embed.fields[1].value.contains(expected_first));
        assert!(embed.fields[1].value.contains(expected_second));
    }
}

#[tokio::test]
async fn migrated_pairing_renders_canonical_wins_in_either_argument_order() {
    assert_migrated_pairing_renders_canonical_wins_in_either_argument_order().await;
}

#[tokio::test]
async fn matchup_uses_server_nickname_then_discord_display_name() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open matchup database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    register(&connection, 1, "AliceStored");
    register(&connection, 2, "BobStored");
    let provider = AdvancedStatsRegistrationProvider::new(database.path()).with_player_names(
        Arc::new(FixedPlayerNames(BTreeMap::from([
            (1, "Alice Server".to_owned()),
            (2, "Bob Global Display".to_owned()),
        ]))),
    );
    let responder = Arc::new(Responder::default());

    registry(&provider)
        .command_handler("matchup")
        .expect("handler")
        .handle(
            request(
                (1, "Alice Global Display"),
                (2, "Bob Global Display"),
                Some(GUILD as u64),
            ),
            responder.clone(),
        )
        .await
        .expect("matchup response");

    assert_eq!(
        responder.followups.lock().expect("followups")[0].embeds[0]
            .title
            .as_deref(),
        Some("Alice Server vs Bob Global Display")
    );
}

#[tokio::test]
async fn test_teammates_only_renders_correct_winrate() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open matchup database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    register(&connection, 1, "Alice");
    register(&connection, 2, "Bob");
    PairingsRepository::new(database.path())
        .update_pairings_for_match(1, Some(GUILD), &[1, 2], &[3, 4], 1)
        .expect("first pairing");
    for match_id in 2..=4 {
        PairingsRepository::new(database.path())
            .update_pairings_for_match(
                match_id,
                Some(GUILD),
                &[1, 2],
                &[3, 4],
                if match_id < 4 { 1 } else { 2 },
            )
            .expect("pairing update");
    }
    let responder = Arc::new(Responder::default());
    registry(&AdvancedStatsRegistrationProvider::new(database.path()))
        .command_handler("matchup")
        .expect("handler")
        .handle(
            request((1, "Alice"), (2, "Bob"), Some(GUILD as u64)),
            responder.clone(),
        )
        .await
        .expect("matchup response");
    let followups = responder.followups.lock().expect("followups");
    assert_eq!(
        followups[0].embeds[0].fields[0].value,
        "4 games, 3 wins (75%)"
    );
    assert_eq!(
        followups[0].embeds[0].fields[1].value,
        "Never played against each other"
    );
}

#[tokio::test]
async fn test_opponents_canonical_order_swap_correct() {
    assert_migrated_pairing_renders_canonical_wins_in_either_argument_order().await;
}

async fn assert_no_history_renders_public_empty_state() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open matchup database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    register(&connection, 1, "Alice");
    register(&connection, 2, "Bob");
    let provider = AdvancedStatsRegistrationProvider::new(database.path());
    let responder = Arc::new(Responder::default());
    registry(&provider)
        .command_handler("matchup")
        .expect("handler")
        .handle(
            request((1, "Alice"), (2, "Bob"), Some(GUILD as u64)),
            responder.clone(),
        )
        .await
        .expect("matchup response");
    let followups = responder.followups.lock().expect("followups");
    assert!(!followups[0].ephemeral);
    assert_eq!(
        followups[0].embeds[0].description.as_deref(),
        Some("No games played together or against each other yet.")
    );
}

#[tokio::test]
async fn no_history_renders_public_empty_state() {
    assert_no_history_renders_public_empty_state().await;
}

#[tokio::test]
async fn test_no_pairing_data_renders_empty_state() {
    assert_no_history_renders_public_empty_state().await;
}

#[test]
fn test_returns_none_when_no_history() {
    let database = database();
    assert_eq!(
        PairingsRepository::new(database.path())
            .get_head_to_head(101, 102, Some(GUILD))
            .expect("head-to-head query"),
        None
    );
}

#[test]
fn test_records_teammate_and_opponent_counts() {
    let database = database();
    let repository = PairingsRepository::new(database.path());
    repository
        .update_pairings_for_match(1, Some(GUILD), &[1, 2], &[3], 1)
        .expect("teammate match one");
    repository
        .update_pairings_for_match(2, Some(GUILD), &[1, 2], &[3], 1)
        .expect("teammate match two");
    for (match_id, winner) in [(3, 1), (4, 2), (5, 2)] {
        repository
            .update_pairings_for_match(match_id, Some(GUILD), &[1], &[2, 3], winner)
            .expect("opponent match");
    }
    let pairing = repository
        .get_head_to_head(1, 2, Some(GUILD))
        .expect("head-to-head query")
        .expect("pairing");
    assert_eq!(pairing.games_together, 2);
    assert_eq!(pairing.wins_together, 2);
    assert_eq!(pairing.games_against, 3);
    assert_eq!(pairing.player1_id, 1);
    assert_eq!(pairing.player1_wins_against, 1);
}
