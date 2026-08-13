//! Writable Survey provider/recovery smoke for an explicitly disposable DB clone.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use cama_app::survey_commands::{GuildSnapshot, RoleSnapshot};
use cama_db::survey::{
    SurveyDeliveryStatus, SurveyQuestionType, SurveyRepository, SurveyTargetType,
};
use cama_runtime::discord_transport::{DiscordMessage, DiscordMessageReceipt};
use cama_runtime::{
    GatewayMember, GuildMemberPageSource, ReadyRecoveryContext, SurveyDiscordPort, SurveyDmError,
    SurveyDmHistory, SurveyRegistrationProvider,
};

const CONFIRMATION: &str = "--disposable-copy";
const SENTINEL_GUILD: i64 = 9_000_000_000_000_001;
const SENTINEL_USER: i64 = 9_000_000_000_000_002;

#[derive(Default)]
struct OfflineSurveyDiscord {
    sends: AtomicUsize,
    edits: AtomicUsize,
}

#[async_trait]
impl SurveyDiscordPort for OfflineSurveyDiscord {
    async fn survey_guild_snapshot(&self, guild_id: i64) -> Result<GuildSnapshot, String> {
        Ok(GuildSnapshot {
            guild_id,
            members: Vec::new(),
        })
    }

    async fn survey_role_snapshot(
        &self,
        _guild_id: i64,
        _role_id: i64,
    ) -> Result<Option<RoleSnapshot>, String> {
        Ok(None)
    }

    async fn survey_send_dm(
        &self,
        discord_id: i64,
        _message: DiscordMessage,
        nonce: &str,
    ) -> Result<DiscordMessageReceipt, SurveyDmError> {
        if discord_id != SENTINEL_USER || !nonce.starts_with("cama-s:") {
            return Err(SurveyDmError {
                kind: cama_runtime::SurveyDmErrorKind::Unavailable,
                message: "unexpected disposable Survey send".to_owned(),
            });
        }
        self.sends.fetch_add(1, Ordering::SeqCst);
        Ok(DiscordMessageReceipt {
            channel_id: 8_000_000_000_000_001,
            message_id: 8_000_000_000_000_002,
            jump_url: String::new(),
        })
    }

    async fn survey_dm_history(
        &self,
        _discord_id: i64,
        _after_unix_seconds: i64,
        _limit: usize,
    ) -> Result<SurveyDmHistory, String> {
        Ok(SurveyDmHistory {
            channel_id: 8_000_000_000_000_001,
            bot_user_id: 8_000_000_000_000_003,
            messages: Vec::new(),
        })
    }

    async fn survey_edit_dm(
        &self,
        _channel_id: i64,
        _message_id: i64,
        _message: DiscordMessage,
    ) -> Result<(), String> {
        self.edits.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct EmptyMembers;

#[async_trait]
impl GuildMemberPageSource for EmptyMembers {
    async fn fetch_page(
        &self,
        _guild_id: u64,
        _after: Option<u64>,
        _limit: u64,
    ) -> Result<Vec<GatewayMember>, String> {
        Ok(Vec::new())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let path = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("usage: survey_snapshot_smoke <db-path> --disposable-copy")?;
    if arguments.next().as_deref() != Some(CONFIRMATION) || arguments.next().is_some() {
        return Err("refusing Survey write smoke without exactly --disposable-copy".into());
    }

    let discord = Arc::new(OfflineSurveyDiscord::default());
    let provider = SurveyRegistrationProvider::new(&path, discord.clone())?;
    let repository = SurveyRepository::new(&path);
    if repository
        .list_surveys(SENTINEL_GUILD, None)?
        .iter()
        .any(|survey| survey.title == "Rust disposable Survey recovery smoke")
    {
        return Err("Survey sentinel already exists; use a fresh disposable copy".into());
    }
    let survey =
        repository.create_survey(SENTINEL_GUILD, "Rust disposable Survey recovery smoke")?;
    repository.add_question(
        SENTINEL_GUILD,
        survey.survey_id,
        "Can the Rust recovery worker deliver this clone-only sentinel?",
        SurveyQuestionType::Nps,
        true,
    )?;
    repository.open_survey(
        SENTINEL_GUILD,
        survey.survey_id,
        &[SENTINEL_USER],
        SurveyTargetType::Member,
        false,
    )?;

    let context = ReadyRecoveryContext::new(
        Arc::<[u64]>::from([u64::try_from(SENTINEL_GUILD)?]),
        Arc::new(EmptyMembers),
    );
    let report = provider.gateway_observer().ready_recovery(context).await;
    if !report.failures.is_empty() {
        return Err(format!("Survey READY recovery failed: {:?}", report.failures).into());
    }
    let session = repository
        .get_response_session(survey.survey_id, SENTINEL_USER)?
        .ok_or("Survey sentinel response disappeared")?;
    if session.recipient.delivery_status != SurveyDeliveryStatus::Sent
        || session.recipient.dm_message_id != Some(8_000_000_000_000_002)
        || discord.sends.load(Ordering::SeqCst) != 1
        || discord.edits.load(Ordering::SeqCst) != 1
    {
        return Err("Survey clone recovery did not persist and reconcile exactly once".into());
    }
    println!(
        "survey_snapshot_smoke=ok survey_id={} sends=1 edits=1 status=sent",
        survey.survey_id
    );
    Ok(())
}
