//! An attached public thread inherits the private parent's visibility. Its ID is
//! the starter message ID, allowing recovery without creating duplicate threads.

use super::*;
use crate::discord_transport::SPECTATOR_THREAD_DELETED;
use serenity::all::{AutoArchiveDuration, CreateThread, EditThread, Message};

fn validate_thread_ids(parent: u64, map: u64, known: Option<u64>) -> Result<(), String> {
    if parent == 0 || map == 0 || parent == map || known.is_some_and(|id| id != map) {
        return Err("Spectator commentary must be attached to its current map message.".into());
    }
    Ok(())
}

fn check_starter(
    message: &Message,
    guild: u64,
    parent: u64,
    map: u64,
    bot: u64,
) -> Result<(), String> {
    if message.id.get() != map
        || message.channel_id.get() != parent
        || message.guild_id.is_some_and(|id| id.get() != guild)
        || message.author.id.get() != bot
        || message.webhook_id.is_some()
    {
        return Err("Spectator map starter is not owned by this bot in its parent channel.".into());
    }
    Ok(())
}

fn check_thread(
    thread: &GuildChannel,
    guild: u64,
    parent: u64,
    map: u64,
    bot: u64,
) -> Result<(), String> {
    validate_thread_ids(parent, map, Some(thread.id.get()))?;
    if thread.guild_id.get() != guild
        || thread.parent_id != Some(ChannelId::new(parent))
        || thread.kind != ChannelType::PublicThread
        || thread.owner_id != Some(UserId::new(bot))
        || thread.thread_metadata.is_none()
    {
        return Err("Spectator commentary thread ownership, parent, or type changed.".into());
    }
    Ok(())
}

fn check_active(thread: &GuildChannel) -> Result<(), String> {
    match &thread.thread_metadata {
        Some(metadata) if !metadata.archived && !metadata.locked => Ok(()),
        _ => Err("Spectator commentary thread is archived, locked, or unverifiable.".into()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn audit_parent_and_starter(
    http: &Arc<Http>,
    guild: u64,
    parent: u64,
    map: u64,
    marker: &str,
    participants: &[u64],
    viewers: &[u64],
) -> Result<u64, String> {
    validate_thread_ids(parent, map, None)?;
    // Membership and administrator checks use REST, as does the parent ACL.
    // Thread membership never substitutes for permission to view the parent.
    audit(http, guild, marker, participants, viewers, parent).await?;
    let bot = http
        .get_current_user()
        .await
        .map_err(|e| e.to_string())?
        .id
        .get();
    let starter = http
        .get_message(ChannelId::new(parent), MessageId::new(map))
        .await
        .map_err(|e| format!("Unable to verify spectator map starter: {e}"))?;
    check_starter(&starter, guild, parent, map, bot)?;
    Ok(bot)
}

#[allow(clippy::too_many_arguments)]
pub(in super::super) async fn ensure_thread(
    http: &Arc<Http>,
    guild: u64,
    parent: u64,
    map: u64,
    marker: &str,
    name: &str,
    participants: &[u64],
    viewers: &[u64],
    known: Option<u64>,
) -> Result<u64, String> {
    validate_thread_ids(parent, map, known)?;
    if !(2..=100).contains(&name.chars().count()) || name.chars().any(char::is_control) {
        return Err("Invalid spectator commentary thread name.".into());
    }
    let bot =
        audit_parent_and_starter(http, guild, parent, map, marker, participants, viewers).await?;
    // Always query by starter ID, even when the durable create response was lost.
    let existing = channel(http, map).await?;
    if existing.is_none() && known.is_some() {
        return Err(SPECTATOR_THREAD_DELETED.into());
    }
    let mut mutated = false;
    let thread = if let Some(existing) = existing {
        check_thread(&existing, guild, parent, map, bot)?;
        if check_active(&existing).is_err() {
            mutated = true;
            ChannelId::new(map)
                .edit_thread(
                    http.as_ref(),
                    EditThread::new().locked(false).archived(false),
                )
                .await
                .map_err(|e| e.to_string())?
        } else {
            existing
        }
    } else {
        mutated = true;
        ChannelId::new(parent)
            .create_thread_from_message(
                http.as_ref(),
                MessageId::new(map),
                CreateThread::new(name)
                    .kind(ChannelType::PublicThread)
                    .auto_archive_duration(AutoArchiveDuration::OneDay),
            )
            .await
            .map_err(|e| e.to_string())?
    };
    check_thread(&thread, guild, parent, map, bot)?;
    check_active(&thread)?;
    // A successful mutation is not enough: permissions may have changed while
    // it was in flight. Do not return a usable destination until freshly audited.
    if mutated {
        audit_thread(http, guild, parent, map, map, marker, participants, viewers).await?;
    }
    Ok(map)
}

#[allow(clippy::too_many_arguments)]
pub(in super::super) async fn audit_thread(
    http: &Arc<Http>,
    guild: u64,
    parent: u64,
    map: u64,
    thread_id: u64,
    marker: &str,
    participants: &[u64],
    viewers: &[u64],
) -> Result<(), String> {
    validate_thread_ids(parent, map, Some(thread_id))?;
    let bot =
        audit_parent_and_starter(http, guild, parent, map, marker, participants, viewers).await?;
    let thread = channel(http, thread_id)
        .await?
        .ok_or("Spectator commentary thread no longer exists.")?;
    check_thread(&thread, guild, parent, map, bot)?;
    check_active(&thread)
}

#[cfg(all(test, feature = "runtime-test-match"))]
mod tests {
    use super::*;

    fn fixture() -> GuildChannel {
        let mut thread = GuildChannel::default();
        thread.id = ChannelId::new(700);
        thread.guild_id = GuildId::new(100);
        thread.parent_id = Some(ChannelId::new(500));
        thread.owner_id = Some(UserId::new(3));
        thread.kind = ChannelType::PublicThread;
        thread.thread_metadata = Some(
            serde_json::from_value(serde_json::json!({
                "archived": false,
                "auto_archive_duration": 1440,
                "locked": false
            }))
            .unwrap(),
        );
        thread
    }

    #[test]
    fn recovery_identity_is_starter_id_with_or_without_persisted_response() {
        assert!(validate_thread_ids(500, 700, None).is_ok());
        assert!(validate_thread_ids(500, 700, Some(700)).is_ok());
        for (parent, map, known) in [
            (0, 700, None),
            (500, 0, None),
            (500, 500, None),
            (500, 700, Some(701)),
        ] {
            assert!(validate_thread_ids(parent, map, known).is_err());
        }
    }

    #[test]
    fn wrong_guild_parent_owner_type_or_starter_cannot_be_recovered_or_reopened() {
        let thread = fixture();
        assert!(check_thread(&thread, 100, 500, 700, 3).is_ok());
        assert!(check_thread(&thread, 101, 500, 700, 3).is_err());
        assert!(check_thread(&thread, 100, 501, 700, 3).is_err());
        assert!(check_thread(&thread, 100, 500, 701, 3).is_err());
        assert!(check_thread(&thread, 100, 500, 700, 4).is_err());
        for kind in [
            ChannelType::PrivateThread,
            ChannelType::NewsThread,
            ChannelType::Text,
        ] {
            let mut foreign = thread.clone();
            foreign.kind = kind;
            assert!(check_thread(&foreign, 100, 500, 700, 3).is_err());
        }
        let mut missing = thread;
        missing.thread_metadata = None;
        assert!(check_thread(&missing, 100, 500, 700, 3).is_err());
    }

    #[test]
    fn locked_or_archived_threads_require_reconciliation_before_delivery() {
        let mut thread = fixture();
        assert!(check_active(&thread).is_ok());
        for (archived, locked) in [(true, false), (false, true), (true, true)] {
            let metadata = thread.thread_metadata.as_mut().unwrap();
            metadata.archived = archived;
            metadata.locked = locked;
            assert!(check_thread(&thread, 100, 500, 700, 3).is_ok());
            assert!(check_active(&thread).is_err());
        }
        let payload =
            serde_json::to_value(EditThread::new().locked(false).archived(false)).unwrap();
        assert_eq!(
            payload,
            serde_json::json!({"locked":false,"archived":false})
        );
    }

    #[test]
    fn starter_must_be_bot_authored_in_the_audited_parent() {
        let mut message = Message::default();
        message.id = MessageId::new(700);
        message.channel_id = ChannelId::new(500);
        message.author.id = UserId::new(3);
        assert!(check_starter(&message, 100, 500, 700, 3).is_ok());
        assert!(check_starter(&message, 100, 501, 700, 3).is_err());
        assert!(check_starter(&message, 100, 500, 701, 3).is_err());
        assert!(check_starter(&message, 100, 500, 700, 4).is_err());
        message.guild_id = Some(GuildId::new(101));
        assert!(check_starter(&message, 100, 500, 700, 3).is_err());
    }
}
