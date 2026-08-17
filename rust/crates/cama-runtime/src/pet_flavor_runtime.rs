//! Shared production glue for the Camagotchi flavor service.
//!
//! The `/pet` command provider and the lifecycle sweep worker compose the
//! same SQLite/AI-backed [`PetFlavorService`]; this module owns the adapters
//! they share. Each caller keeps its own runtime wrapper so the provider's
//! synchronous path and the worker's blocking-pool path stay unchanged.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cama_app::ai_services::{
    AIService, Message, MessageRole, ToolChoice, ToolDefinition, ToolProperty, ToolPropertySchema,
    ToolRequest, Value as AiValue,
};
use cama_app::pet_assets::EvolutionVisual;
use cama_app::pet_flavor::{
    BUNDLE_TOOL_NAME, FlavorClock, FlavorDataPort, FlavorRng, GuildAiPort, LINE_TOOL_NAME,
    LedgerEntry, LlmPort, LlmRequest, PetFlavorService, ToolCallResult as FlavorToolCallResult,
    ToolValue,
};
use cama_db::core_repositories::PlayerRepository;
use cama_db::guild_config_repository::GuildConfigRepository;
use cama_domain::guild_config::GuildConfigStore;
use cama_domain::pet::Pet;
use cama_domain::pet_evolution::{PetCalling, PetInstinct};
use chrono::Utc;
use rusqlite::{Connection, params};

/// Compose the production flavor service over an admitted SQLite database.
pub(crate) fn production_pet_flavor_service(
    database_path: impl AsRef<Path>,
    ai_service: Option<Arc<AIService>>,
    ai_features_default: bool,
) -> Arc<PetFlavorService> {
    let database_path = database_path.as_ref().to_path_buf();
    let ai =
        ai_service.map(|service| Arc::new(ProductionPetFlavorLlm(service)) as Arc<dyn LlmPort>);
    Arc::new(PetFlavorService::new(
        ai,
        Some(Arc::new(ProductionPetGuildAi(GuildConfigRepository::new(
            &database_path,
            ai_features_default,
        )))),
        Some(Arc::new(ProductionPetFlavorData::new(&database_path))),
        Arc::new(SystemPetFlavorClock),
        Box::new(ProductionPetFlavorRng),
    ))
}

#[derive(Clone)]
struct ProductionPetFlavorLlm(Arc<AIService>);

impl LlmPort for ProductionPetFlavorLlm {
    fn call_with_tools(&self, request: LlmRequest) -> Result<FlavorToolCallResult, String> {
        let definition = pet_flavor_tool(request.tool_name)?;
        let messages = request
            .messages
            .into_iter()
            .map(|message| {
                let role = match message.role {
                    "system" => MessageRole::System,
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "tool" => MessageRole::Tool,
                    other => return Err(format!("unsupported pet flavor message role {other}")),
                };
                Ok(Message::new(role, message.content))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let max_tokens = u32::try_from(request.max_tokens)
            .map_err(|_| format!("invalid pet flavor max token count {}", request.max_tokens))?;
        let result = self.0.call_with_tools(ToolRequest {
            messages,
            tools: vec![definition],
            tool_choice: ToolChoice::Required(request.tool_name.to_owned()),
            max_tokens: Some(max_tokens),
            temperature: Some(request.temperature),
            feature: request.feature,
        });
        let tool_name = result
            .tool_name
            .ok_or_else(|| "pet flavor provider returned no tool call".to_owned())?;
        Ok(FlavorToolCallResult {
            tool_name,
            tool_args: ToolValue::Object(
                result
                    .tool_args
                    .into_iter()
                    .map(|(key, value)| (key, pet_tool_value(value)))
                    .collect(),
            ),
        })
    }
}

#[derive(Clone)]
struct ProductionPetGuildAi(GuildConfigRepository);

impl GuildAiPort for ProductionPetGuildAi {
    fn ai_enabled(&self, guild_id: i64) -> Result<bool, String> {
        self.0
            .get_ai_enabled(guild_id)
            .map_err(|error| error.to_string())
    }
}

/// SQLite-backed finance context used by the production pet flavor service.
/// The public read helper below is the disposable-snapshot boundary.
#[derive(Clone)]
struct ProductionPetFlavorData {
    database_path: PathBuf,
    players: PlayerRepository,
}

impl ProductionPetFlavorData {
    fn new(database_path: impl AsRef<Path>) -> Self {
        Self {
            database_path: database_path.as_ref().to_path_buf(),
            players: PlayerRepository::new(database_path),
        }
    }

    fn connection(&self) -> Result<Connection, String> {
        let connection =
            Connection::open(&self.database_path).map_err(|error| error.to_string())?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| error.to_string())?;
        Ok(connection)
    }
}

/// Read the production pet flavor finance context from an admitted SQLite
/// database. The function performs no migration or writes.
pub fn read_production_pet_flavor_context(
    database_path: impl AsRef<Path>,
    discord_id: i64,
    guild_id: i64,
    limit: usize,
) -> Result<(i64, Vec<LedgerEntry>), String> {
    let data = ProductionPetFlavorData::new(database_path);
    let balance = data.balance(discord_id, guild_id)?;
    let entries = data.recent_entries(guild_id, discord_id, limit)?;
    Ok((balance, entries))
}

impl FlavorDataPort for ProductionPetFlavorData {
    fn balance(&self, discord_id: i64, guild_id: i64) -> Result<i64, String> {
        self.players
            .get_by_id(discord_id, Some(guild_id))
            .map_err(|error| error.to_string())?
            .map(|player| player.jopacoin_balance)
            .ok_or_else(|| "pet flavor player was not found".to_owned())
    }

    fn recent_entries(
        &self,
        guild_id: i64,
        discord_id: i64,
        limit: usize,
    ) -> Result<Vec<LedgerEntry>, String> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT CAST(delta AS TEXT),source,COALESCE(reason,''),COALESCE(metadata,'')
                 FROM economy_ledger_entries
                 WHERE guild_id=?1 AND account_type='player' AND account_id=?2
                 ORDER BY created_at DESC,ledger_id DESC LIMIT ?3",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map(params![guild_id, discord_id, limit], |row| {
                Ok(LedgerEntry {
                    delta: row.get(0)?,
                    source: row.get(1)?,
                    reason: row.get(2)?,
                    metadata: row.get(3)?,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }
}

struct SystemPetFlavorClock;

impl FlavorClock for SystemPetFlavorClock {
    fn now(&self) -> i64 {
        Utc::now().timestamp()
    }
}

struct ProductionPetFlavorRng;

impl FlavorRng for ProductionPetFlavorRng {
    fn choose_index(&mut self, len: usize) -> usize {
        if len == 0 { 0 } else { fastrand::usize(..len) }
    }
}

fn pet_flavor_tool(name: &str) -> Result<ToolDefinition, String> {
    let (description, properties) = match name {
        BUNDLE_TOOL_NAME => (
            "Generate a reusable bundle of cheerful Camagotchi quips.",
            [("status", 6), ("fed", 5), ("spat", 3)]
                .into_iter()
                .map(|(name, count)| ToolProperty {
                    name: name.to_owned(),
                    description: String::new(),
                    enum_values: Vec::new(),
                    schema: ToolPropertySchema::StringArray {
                        min_items: count,
                        max_items: count,
                        unique_items: true,
                    },
                })
                .collect(),
        ),
        LINE_TOOL_NAME => (
            "Generate one safe, lighthearted Camagotchi line.",
            vec![ToolProperty {
                name: "line".to_owned(),
                description: String::new(),
                enum_values: Vec::new(),
                schema: ToolPropertySchema::String,
            }],
        ),
        other => return Err(format!("unsupported pet flavor tool {other}")),
    };
    Ok(ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        required: properties
            .iter()
            .map(|property| property.name.clone())
            .collect(),
        properties,
        additional_properties: Some(false),
    })
}

fn pet_tool_value(value: AiValue) -> ToolValue {
    match value {
        AiValue::Null => ToolValue::Null,
        AiValue::Text(value) => ToolValue::Text(value),
        AiValue::List(values) => ToolValue::Array(values.into_iter().map(pet_tool_value).collect()),
        AiValue::Object(values) => ToolValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, pet_tool_value(value)))
                .collect(),
        ),
        AiValue::Bool(value) => ToolValue::Text(value.to_string()),
        AiValue::Integer(value) => ToolValue::Text(value.to_string()),
        AiValue::Real(value) => ToolValue::Text(value.to_string()),
    }
}

pub(crate) fn evolution_visual(pet: &Pet) -> Option<EvolutionVisual> {
    let calling = parse_calling(pet.evolution_calling.as_deref()?)?;
    let primary = parse_instinct(pet.evolution_primary.as_deref()?)?;
    Some(EvolutionVisual {
        calling,
        primary,
        secondary: pet.evolution_secondary.as_deref().and_then(parse_instinct),
    })
}

fn parse_calling(value: &str) -> Option<PetCalling> {
    PetCalling::ALL
        .into_iter()
        .find(|calling| calling.as_str() == value)
}

fn parse_instinct(value: &str) -> Option<PetInstinct> {
    PetInstinct::ALL
        .into_iter()
        .find(|instinct| instinct.as_str() == value)
}
