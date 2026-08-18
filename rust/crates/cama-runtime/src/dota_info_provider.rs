//! Live `/dota hero` and `/dota ability` Discord provider.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use cama_app::dota_info::{DotaInfoEmbed, DotaInfoService, DotaInfoSource};
use cama_app::dotabase_sqlite::{DotabaseSqliteSource, PRODUCTION_DOTABASE_PATH};

use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, InteractionEmbed,
    InteractionHandler, InteractionHandlerError, InteractionRequest, InteractionResponder,
    InteractionResponse, InteractionValue, RegistrationError, RegistrationProvider,
    RegistryBuilder,
};

pub struct DotaInfoRegistrationProvider<S = DotabaseSqliteSource> {
    handler: Arc<DotaInfoHandler<S>>,
}

impl DotaInfoRegistrationProvider<DotabaseSqliteSource> {
    pub fn production() -> Result<Self, String> {
        Self::from_path(PRODUCTION_DOTABASE_PATH)
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, String> {
        let service = Arc::new(DotaInfoService::new(DotabaseSqliteSource::new(path)));
        service
            .hero_autocomplete("")
            .map_err(|error| error.to_string())?;
        service
            .ability_autocomplete("")
            .map_err(|error| error.to_string())?;
        if service
            .hero("Pudge")
            .map_err(|error| error.to_string())?
            .is_none()
        {
            return Err("bundled Dotabase catalog is missing Pudge".to_owned());
        }
        if service
            .ability("Meat Hook")
            .map_err(|error| error.to_string())?
            .is_none()
        {
            return Err("bundled Dotabase catalog is missing Meat Hook".to_owned());
        }
        Ok(Self::from_service(service))
    }
}

impl<S: DotaInfoSource> DotaInfoRegistrationProvider<S> {
    #[must_use]
    pub fn from_service(service: Arc<DotaInfoService<S>>) -> Self {
        Self {
            handler: Arc::new(DotaInfoHandler { service }),
        }
    }
}

impl<S: DotaInfoSource> RegistrationProvider for DotaInfoRegistrationProvider<S> {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "dota".to_owned(),
            description: "Dota 2 hero and ability reference".to_owned(),
            options: vec![
                CommandOptionSpec::new(
                    "hero",
                    "Get information about a Dota 2 hero",
                    CommandOptionKind::Subcommand,
                )
                .options(vec![
                    CommandOptionSpec::new(
                        "hero_name",
                        "The hero name (e.g., Anti-Mage, Pudge)",
                        CommandOptionKind::String,
                    )
                    .required(true)
                    .autocomplete(),
                ]),
                CommandOptionSpec::new(
                    "ability",
                    "Get information about a Dota 2 ability",
                    CommandOptionKind::Subcommand,
                )
                .options(vec![
                    CommandOptionSpec::new(
                        "ability_name",
                        "The ability name (e.g., Blink, Mana Break)",
                        CommandOptionKind::String,
                    )
                    .required(true)
                    .autocomplete(),
                ]),
            ],
            handler: self.handler.clone(),
        })
    }
}

struct DotaInfoHandler<S> {
    service: Arc<DotaInfoService<S>>,
}

#[async_trait]
impl<S: DotaInfoSource> InteractionHandler for DotaInfoHandler<S> {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Autocomplete {
                focused_option,
                focused_value,
                ..
            } => {
                let service = Arc::clone(&self.service);
                let choices = tokio::task::spawn_blocking(move || match focused_option.as_str() {
                    "hero_name" => service.hero_autocomplete(&focused_value),
                    "ability_name" => service.ability_autocomplete(&focused_value),
                    _ => Ok(Vec::new()),
                })
                .await
                .map_err(|error| InteractionHandlerError::Handler(error.to_string()))?
                .map_err(|error| InteractionHandlerError::Handler(error.to_string()))?;
                responder
                    .autocomplete(
                        choices
                            .into_iter()
                            .map(|choice| CommandOptionChoice::String {
                                name: choice.name,
                                value: choice.value,
                            })
                            .collect(),
                    )
                    .await
                    .map_err(|error| InteractionHandlerError::Handler(error.to_string()))
            }
            InteractionRequest::Command { options, .. } => {
                let (kind, query) = command_query(&options).ok_or_else(|| {
                    InteractionHandlerError::Handler("invalid /dota command payload".to_owned())
                })?;
                responder
                    .defer(false)
                    .await
                    .map_err(|error| InteractionHandlerError::Handler(error.to_string()))?;
                let service = Arc::clone(&self.service);
                let query_for_lookup = query.clone();
                let embed = tokio::task::spawn_blocking(move || match kind {
                    DotaInfoKind::Hero => service.hero(&query_for_lookup),
                    DotaInfoKind::Ability => service.ability(&query_for_lookup),
                })
                .await
                .map_err(|error| InteractionHandlerError::Handler(error.to_string()))?
                .map_err(|error| InteractionHandlerError::Handler(error.to_string()))?;
                let response = match embed {
                    Some(embed) => InteractionResponse::message("").embed(interaction_embed(embed)),
                    None => InteractionResponse::message(format!(
                        "{} '{query}' not found. Try using the autocomplete suggestions.",
                        match kind {
                            DotaInfoKind::Hero => "Hero",
                            DotaInfoKind::Ability => "Ability",
                        }
                    )),
                };
                responder
                    .followup(response)
                    .await
                    .map_err(|error| InteractionHandlerError::Handler(error.to_string()))
            }
            InteractionRequest::Component { .. } | InteractionRequest::Modal { .. } => Err(
                InteractionHandlerError::Handler("/dota received a component interaction".into()),
            ),
        }
    }
}

#[derive(Clone, Copy)]
enum DotaInfoKind {
    Hero,
    Ability,
}

fn command_query(
    options: &[crate::registration::InteractionOption],
) -> Option<(DotaInfoKind, String)> {
    options.iter().find_map(|option| {
        let InteractionValue::Subcommand(children) = &option.value else {
            return None;
        };
        let kind = match option.name.as_str() {
            "hero" => DotaInfoKind::Hero,
            "ability" => DotaInfoKind::Ability,
            _ => return None,
        };
        children.iter().find_map(|child| match &child.value {
            InteractionValue::String(value) => Some((kind, value.clone())),
            _ => None,
        })
    })
}

fn interaction_embed(embed: DotaInfoEmbed) -> InteractionEmbed {
    let mut converted = InteractionEmbed::titled(embed.title).color(embed.color);
    if let Some(description) = embed.description {
        converted = converted.description(description);
    }
    if let Some(thumbnail) = embed.thumbnail_url {
        converted = converted.thumbnail(thumbnail);
    }
    if let Some(footer) = embed.footer {
        converted = converted.footer(footer);
    }
    for field in embed.fields {
        converted = converted.field(field.name, field.value, field.inline);
    }
    converted
}

#[cfg(all(test, feature = "runtime-test-match"))]
#[path = "dota_info_provider/tests.rs"]
mod tests;
