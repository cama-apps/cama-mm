use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use cama_app::dota_info::{DotaAbility, DotaHero, DotaInfoError};

use super::*;
use crate::registration::{
    InteractionOption, InteractionResponseError, InteractionValue, RegistryBuilder,
};

#[derive(Clone, Default)]
struct FixtureSource {
    calls: Arc<Mutex<Vec<(String, std::thread::ThreadId)>>>,
}

impl FixtureSource {
    fn record(&self, name: &str) {
        self.calls
            .lock()
            .unwrap()
            .push((name.to_owned(), std::thread::current().id()));
    }
}

impl DotaInfoSource for FixtureSource {
    fn all_heroes(&self) -> Result<Vec<(String, i64)>, DotaInfoError> {
        self.record("all_heroes");
        Ok(vec![("Pudge".to_owned(), 14)])
    }

    fn all_abilities(&self) -> Result<Vec<(String, i64)>, DotaInfoError> {
        self.record("all_abilities");
        Ok(vec![("Meat Hook".to_owned(), 14)])
    }

    fn hero_by_name(&self, _name: &str) -> Result<Option<DotaHero>, DotaInfoError> {
        self.record("hero_by_name");
        Ok(None)
    }

    fn ability_by_name(&self, _name: &str) -> Result<Option<DotaAbility>, DotaInfoError> {
        self.record("ability_by_name");
        Ok(None)
    }
}

#[derive(Default)]
struct CapturingResponder {
    deferred: AtomicBool,
    autocomplete: Mutex<Vec<CommandOptionChoice>>,
    followups: Mutex<Vec<InteractionResponse>>,
}

#[async_trait]
impl InteractionResponder for CapturingResponder {
    async fn respond(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        Ok(())
    }

    async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.deferred.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.followups.lock().unwrap().push(response);
        Ok(())
    }

    async fn autocomplete(
        &self,
        choices: Vec<CommandOptionChoice>,
    ) -> Result<(), InteractionResponseError> {
        *self.autocomplete.lock().unwrap() = choices;
        Ok(())
    }
}

fn handler(source: FixtureSource) -> Arc<dyn InteractionHandler> {
    let provider =
        DotaInfoRegistrationProvider::from_service(Arc::new(DotaInfoService::new(source)));
    let mut registry = RegistryBuilder::default();
    registry.add_provider(&provider).unwrap();
    registry.build().command_handler("dota").unwrap()
}

fn detail_request(kind: &str, option: &str, value: &str) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "dota".to_owned(),
        user_id: 2,
        user_display_name: "Player".to_owned(),
        guild_id: Some(3),
        channel_id: Some(4),
        member_permissions: None,
        options: vec![InteractionOption {
            name: kind.to_owned(),
            value: InteractionValue::Subcommand(vec![InteractionOption {
                name: option.to_owned(),
                value: InteractionValue::String(value.to_owned()),
            }]),
        }],
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_autocomplete_loaders_are_offloaded_from_the_event_loop() {
    let caller = std::thread::current().id();
    let source = FixtureSource::default();
    let handler = handler(source.clone());
    for (focused_option, focused_value, expected) in [
        ("hero_name", "pud", "Pudge"),
        ("ability_name", "hook", "Meat Hook"),
    ] {
        let responder = Arc::new(CapturingResponder::default());
        handler
            .handle(
                InteractionRequest::Autocomplete {
                    interaction_id: 1,
                    name: "dota".to_owned(),
                    user_id: 2,
                    guild_id: Some(3),
                    channel_id: Some(4),
                    focused_option: focused_option.to_owned(),
                    focused_value: focused_value.to_owned(),
                    options: Vec::new(),
                },
                responder.clone(),
            )
            .await
            .unwrap();
        let choices = responder.autocomplete.lock().unwrap();
        assert!(matches!(
            choices.as_slice(),
            [CommandOptionChoice::String { name, value }] if name == expected && value == expected
        ));
    }
    assert!(
        source
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(_, thread)| *thread != caller)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_detail_lookups_are_offloaded_from_the_event_loop_hero() {
    assert_detail_is_offloaded("hero", "hero_name", "Pudge", "Hero 'Pudge' not found").await;
}

#[tokio::test(flavor = "current_thread")]
async fn test_detail_lookups_are_offloaded_from_the_event_loop_ability() {
    assert_detail_is_offloaded(
        "ability",
        "ability_name",
        "Meat Hook",
        "Ability 'Meat Hook' not found",
    )
    .await;
}

async fn assert_detail_is_offloaded(kind: &str, option: &str, value: &str, expected: &str) {
    let caller = std::thread::current().id();
    let source = FixtureSource::default();
    let responder = Arc::new(CapturingResponder::default());
    handler(source.clone())
        .handle(detail_request(kind, option, value), responder.clone())
        .await
        .unwrap();
    assert!(responder.deferred.load(Ordering::Relaxed));
    assert!(
        responder.followups.lock().unwrap()[0]
            .content
            .contains(expected)
    );
    assert!(
        source
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(_, thread)| *thread != caller)
    );
}
