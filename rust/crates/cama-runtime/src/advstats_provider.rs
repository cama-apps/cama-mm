//! Live `/matchup` head-to-head statistics command.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use cama_db::core_repositories::PlayerRepository;
use cama_db::pairings_repository::{Pairing, PairingsRepository};

use crate::discord_transport::{GuildPlayerNameResolver, StoredUsernamePlayerNameResolver};
use crate::registration::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, InteractionEmbed, InteractionHandler,
    InteractionHandlerError, InteractionOption, InteractionRequest, InteractionResponder,
    InteractionResponse, InteractionValue, RegistrationError, RegistrationProvider,
    RegistryBuilder,
};

#[derive(Clone)]
pub struct AdvancedStatsRegistrationProvider {
    handler: Arc<AdvancedStatsHandler>,
}

impl AdvancedStatsRegistrationProvider {
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>) -> Self {
        Self {
            handler: Arc::new(AdvancedStatsHandler {
                players: PlayerRepository::new(database_path.as_ref()),
                pairings: PairingsRepository::new(database_path),
                player_names: Arc::new(StoredUsernamePlayerNameResolver),
            }),
        }
    }

    #[must_use]
    pub fn with_player_names(mut self, player_names: Arc<dyn GuildPlayerNameResolver>) -> Self {
        Arc::get_mut(&mut self.handler)
            .expect("new advanced-stats provider owns its handler")
            .player_names = player_names;
        self
    }
}

impl RegistrationProvider for AdvancedStatsRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "matchup".to_owned(),
            description: "View head-to-head stats between two players".to_owned(),
            options: vec![
                CommandOptionSpec::new("player1", "First player", CommandOptionKind::User)
                    .required(true),
                CommandOptionSpec::new("player2", "Second player", CommandOptionKind::User)
                    .required(true),
            ],
            handler: self.handler.clone(),
        })
    }
}

struct AdvancedStatsHandler {
    players: PlayerRepository,
    pairings: PairingsRepository,
    player_names: Arc<dyn GuildPlayerNameResolver>,
}

#[derive(Clone)]
struct MatchupUser {
    id: i64,
    display_name: String,
}

#[async_trait]
impl InteractionHandler for AdvancedStatsHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let InteractionRequest::Command {
            name,
            guild_id,
            options,
            ..
        } = request
        else {
            return Err("matchup accepts slash-command interactions only".into());
        };
        if name != "matchup" {
            return Err(format!("advanced-stats handler received command {name:?}").into());
        }
        let Some(guild_id) = guild_id else {
            return respond(
                &responder,
                InteractionResponse::message("This command can only be used in a server.")
                    .ephemeral(),
            )
            .await;
        };
        let guild_id = i64::try_from(guild_id)
            .map_err(|_| InteractionHandlerError::from("Discord guild ID exceeds SQLite range"))?;
        let mut first = matchup_user(&options, "player1")?;
        let mut second = matchup_user(&options, "player2")?;
        responder.defer(false).await.map_err(|error| {
            InteractionHandlerError::from(format!("matchup defer failed: {error}"))
        })?;
        if first.id == second.id {
            return followup(
                &responder,
                InteractionResponse::message("Cannot compare a player with themselves!")
                    .ephemeral(),
            )
            .await;
        }

        let players = self.players.clone();
        let first_id = first.id;
        let second_id = second.id;
        let registered = tokio::task::spawn_blocking(move || {
            players
                .get_by_ids(&[first_id, second_id], Some(guild_id))
                .map(|players| {
                    players
                        .into_iter()
                        .filter_map(|player| {
                            player
                                .discord_id
                                .map(|discord_id| (discord_id, player.name))
                        })
                        .collect::<BTreeMap<_, _>>()
                })
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| {
            InteractionHandlerError::from(format!("matchup player task failed: {error}"))
        })?
        .map_err(InteractionHandlerError::from)?;
        let names = self
            .player_names
            .names_for_guild(guild_id, &[first.id, second.id])
            .unwrap_or_default();
        if let Some(stored_name) = registered.get(&first.id) {
            first.display_name = names.resolve(first.id, stored_name).to_owned();
        } else {
            first.display_name = names.resolve(first.id, &first.display_name).to_owned();
        }
        if let Some(stored_name) = registered.get(&second.id) {
            second.display_name = names.resolve(second.id, stored_name).to_owned();
        } else {
            second.display_name = names.resolve(second.id, &second.display_name).to_owned();
        }
        if !registered.contains_key(&first.id) {
            return followup(
                &responder,
                InteractionResponse::message(format!("{} is not registered.", first.display_name))
                    .ephemeral(),
            )
            .await;
        }
        if !registered.contains_key(&second.id) {
            return followup(
                &responder,
                InteractionResponse::message(format!("{} is not registered.", second.display_name))
                    .ephemeral(),
            )
            .await;
        }

        let pairings = self.pairings.clone();
        let h2h = tokio::task::spawn_blocking(move || {
            pairings
                .get_head_to_head(first_id, second_id, Some(guild_id))
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| {
            InteractionHandlerError::from(format!("matchup pairing task failed: {error}"))
        })?
        .map_err(InteractionHandlerError::from)?;
        followup(
            &responder,
            InteractionResponse::message("").embed(matchup_embed(&first, &second, h2h.as_ref())),
        )
        .await
    }
}

fn matchup_user(
    options: &[InteractionOption],
    name: &str,
) -> Result<MatchupUser, InteractionHandlerError> {
    options
        .iter()
        .find_map(|option| {
            (option.name == name)
                .then_some(&option.value)
                .and_then(|value| match value {
                    InteractionValue::User { id, .. } => Some(*id),
                    _ => None,
                })
        })
        .ok_or_else(|| InteractionHandlerError::from(format!("/matchup requires {name}")))
        .and_then(|id| {
            Ok(MatchupUser {
                id: i64::try_from(id).map_err(|_| {
                    InteractionHandlerError::from("Discord user ID exceeds SQLite range")
                })?,
                display_name: format!("User {id}"),
            })
        })
}

fn matchup_embed(
    first: &MatchupUser,
    second: &MatchupUser,
    h2h: Option<&Pairing>,
) -> InteractionEmbed {
    let mut embed =
        InteractionEmbed::titled(format!("{} vs {}", first.display_name, second.display_name))
            .color(0xE67E22);
    let Some(h2h) = h2h else {
        return embed.description("No games played together or against each other yet.");
    };
    let teammates = if h2h.games_together > 0 {
        format!(
            "{} games, {} wins ({:.0}%)",
            h2h.games_together,
            h2h.wins_together,
            h2h.wins_together as f64 / h2h.games_together as f64 * 100.0
        )
    } else {
        "Never played together".to_owned()
    };
    embed = embed.field("As Teammates", teammates, false);
    let opponents = if h2h.games_against > 0 {
        let first_wins = h2h.queried_player_wins_against(first.id);
        let second_wins = h2h.games_against - first_wins;
        format!(
            "{} games\n{}: {} wins\n{}: {} wins",
            h2h.games_against, first.display_name, first_wins, second.display_name, second_wins
        )
    } else {
        "Never played against each other".to_owned()
    };
    embed.field("As Opponents", opponents, false)
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

async fn followup(
    responder: &Arc<dyn InteractionResponder>,
    response: InteractionResponse,
) -> Result<(), InteractionHandlerError> {
    responder
        .followup(response)
        .await
        .map_err(|error| InteractionHandlerError::from(error.to_string()))
}

#[cfg(all(test, feature = "runtime-test-match"))]
#[path = "advstats_provider/tests.rs"]
mod tests;
