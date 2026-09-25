//! Private spectator threads use existing text-channel permissions, never ACL edits.
use super::*;
use serenity::all::{ApplicationFlags, AutoArchiveDuration, CreateThread, EditThread};

fn required_permissions() -> Permissions {
    read_permissions()
        | Permissions::SEND_MESSAGES
        | Permissions::SEND_MESSAGES_IN_THREADS
        | Permissions::EMBED_LINKS
        | Permissions::ATTACH_FILES
        | Permissions::CREATE_PRIVATE_THREADS
        | Permissions::MANAGE_THREADS
}

fn thread_name(guild: u64, marker: &str) -> Result<String, String> {
    let parts: Vec<_> = marker.split(':').collect();
    if parts.len() != 4
        || parts[0] != "cama-spectator"
        || parts[1].parse::<u64>().ok() != Some(guild)
        || parts[2].parse::<u64>().ok().is_none_or(|id| id == 0)
        || parts[3].len() != 32
        || !parts[3].bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err("Invalid private spectator thread ownership marker.".into());
    }
    Ok(format!("match-{}-spectators-{}", parts[2], parts[3]))
}

pub(super) fn owned(
    thread: &GuildChannel,
    guild: u64,
    marker: &str,
    bot: u64,
) -> Result<(), String> {
    if thread.guild_id.get() != guild
        || thread.kind != ChannelType::PrivateThread
        || thread.owner_id != Some(UserId::new(bot))
        || thread.name != thread_name(guild, marker)?
        || thread.parent_id.is_none()
    {
        return Err("Private spectator thread ownership, guild, or type changed.".into());
    }
    Ok(())
}

fn active(thread: &GuildChannel) -> Result<(), String> {
    if !thread
        .thread_metadata
        .as_ref()
        .is_some_and(|m| !m.archived && !m.locked && !m.invitable)
    {
        return Err("Spectator thread is archived, locked, or allows member invites.".into());
    }
    Ok(())
}

fn participant_can_read(owner: u64, member: u64, permissions: Permissions) -> bool {
    bypasses_overwrites(owner, member, permissions)
        || permissions.contains(Permissions::VIEW_CHANNEL | Permissions::MANAGE_THREADS)
}

fn check_viewer_permissions(
    permissions: Permissions,
    viewer: u64,
    parent: u64,
) -> Result<(), String> {
    // Watching the map and bot commentary does not require permission to chat.
    // Keep Discord's actual visibility requirements; never change parent ACLs.
    let missing = [
        (Permissions::VIEW_CHANNEL, "View Channel"),
        (Permissions::READ_MESSAGE_HISTORY, "Read Message History"),
    ]
    .into_iter()
    .filter_map(|(permission, name)| (!permissions.contains(permission)).then_some(name))
    .collect::<Vec<_>>()
    .join(", ");
    if !missing.is_empty() {
        return Err(format!(
            "Spectator {viewer} cannot read the spectator thread: missing {missing} in parent channel {parent}."
        ));
    }
    Ok(())
}

async fn parent_channel(http: &Http, guild: u64, source: u64) -> Result<GuildChannel, String> {
    let mut parent = channel(http, source)
        .await?
        .ok_or("Match source channel no longer exists.")?;
    if parent.guild_id.get() != guild {
        return Err("Spectator source belongs to a different guild.".into());
    }
    if matches!(
        parent.kind,
        ChannelType::PublicThread | ChannelType::PrivateThread | ChannelType::NewsThread
    ) {
        parent = channel(
            http,
            parent.parent_id.ok_or("Match thread has no parent.")?.get(),
        )
        .await?
        .ok_or("Match parent channel no longer exists.")?;
    }
    if parent.guild_id.get() != guild || parent.kind != ChannelType::Text {
        return Err(
            "Private spectators require an existing text channel as the match parent.".into(),
        );
    }
    Ok(parent)
}

// All roles and overwrites come from REST, not the gateway cache. A player with
// moderator access cannot be excluded by private thread membership.
async fn policy(
    http: &Arc<Http>,
    guild: u64,
    parent: &GuildChannel,
    participants: &[u64],
    viewers: &[u64],
) -> Result<(u64, BTreeSet<u64>), String> {
    let bot = http
        .get_current_user()
        .await
        .map_err(|e| e.to_string())?
        .id
        .get();
    let (participants, viewers) = roster(bot, participants, viewers)?;
    let app = http
        .get_current_application_info()
        .await
        .map_err(|e| e.to_string())?;
    if !app.flags.is_some_and(|flags| {
        flags.intersects(
            ApplicationFlags::GATEWAY_GUILD_MEMBERS
                | ApplicationFlags::GATEWAY_GUILD_MEMBERS_LIMITED,
        )
    }) {
        return Err("Private spectator membership cannot be audited without the existing guild-members intent.".into());
    }
    let data = http
        .get_guild(GuildId::new(guild))
        .await
        .map_err(|e| e.to_string())?;
    let mut requests = tokio::task::JoinSet::new();
    for id in participants.iter().chain(&viewers).copied().chain([bot]) {
        let http = Arc::clone(http);
        requests.spawn(async move {
            (
                id,
                http.get_member(GuildId::new(guild), UserId::new(id)).await,
            )
        });
    }
    while let Some(result) = requests.join_next().await {
        let (id, member) = result.map_err(|e| e.to_string())?;
        let member = member.map_err(|e| format!("Unable to verify spectator member {id}: {e}"))?;
        if member.user.id.get() != id
            || member.guild_id.get() != guild
            || !data.roles.contains_key(&RoleId::new(guild))
            || member
                .roles
                .iter()
                .any(|role| !data.roles.contains_key(role))
        {
            return Err("Spectator member roles could not be verified.".into());
        }
        let permissions = data.user_permissions_in(parent, &member);
        if participants.contains(&id) && participant_can_read(data.owner_id.get(), id, permissions)
        {
            return Err("A match participant owns this server or has Administrator/Manage Threads access; private spectator threads cannot hide live information from them.".into());
        }
        if id == bot && !permissions.contains(required_permissions()) {
            return Err(format!(
                "Private spectator thread permissions missing in the match parent: {:?}",
                required_permissions() & !permissions
            ));
        }
        if viewers.contains(&id) {
            check_viewer_permissions(permissions, id, parent.id.get())?;
        }
    }
    Ok((bot, viewers))
}

async fn members(http: &Http, thread: ChannelId) -> Result<BTreeSet<u64>, String> {
    // Serenity uses API v10 without with_member: the endpoint returns all members.
    // Fail closed at the pagination boundary as well, for future API changes.
    let members = thread
        .get_thread_members(http)
        .await
        .map_err(|e| e.to_string())?;
    if members.len() >= 100 || members.iter().any(|m| m.id != thread) {
        return Err("Spectator thread membership is incomplete or oversized.".into());
    }
    Ok(members.into_iter().map(|m| m.user_id.get()).collect())
}

fn check_members(actual: &BTreeSet<u64>, bot: u64, viewers: &BTreeSet<u64>) -> Result<(), String> {
    // Opt-ins join through the worker’s durable silent mention. Missing viewers
    // are safe; an unexpected member must block all live delivery.
    if actual.iter().any(|id| *id != bot && !viewers.contains(id)) {
        return Err("An uninvited member has access to the spectator thread.".into());
    }
    Ok(())
}

async fn discover(
    http: &Http,
    guild: u64,
    parent: ChannelId,
    marker: &str,
    bot: u64,
) -> Result<Vec<GuildChannel>, String> {
    let mut found: Vec<_> = GuildId::new(guild)
        .get_active_threads(http)
        .await
        .map_err(|e| e.to_string())?
        .threads
        .into_iter()
        .filter(|t| t.parent_id == Some(parent) && owned(t, guild, marker, bot).is_ok())
        .collect();
    let mut before = None;
    // Includes archived creates after a restart; bounded scans never authorize
    // a replacement when discovery is incomplete.
    for _ in 0..20 {
        let page = parent
            .get_joined_archived_private_threads(http, before, Some(100))
            .await
            .map_err(|e| e.to_string())?;
        let next = page.threads.iter().map(|t| t.id.get()).min();
        found.extend(
            page.threads
                .into_iter()
                .filter(|t| t.parent_id == Some(parent) && owned(t, guild, marker, bot).is_ok()),
        );
        if !page.has_more {
            found.sort_by_key(|t| t.id);
            found.dedup_by_key(|t| t.id);
            return Ok(found);
        }
        if next.is_none() || next == before {
            break;
        }
        before = next;
    }
    Err("Private spectator thread recovery scan is incomplete.".into())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn ensure(
    http: &Arc<Http>,
    guild: u64,
    source: u64,
    marker: &str,
    participants: &[u64],
    viewers: &[u64],
    known: Option<u64>,
) -> Result<u64, String> {
    let name = thread_name(guild, marker)?;
    let parent = parent_channel(http, guild, source).await?;
    let (bot, viewers_set) = policy(http, guild, &parent, participants, viewers).await?;
    let existing = if let Some(id) = known {
        channel(http, id).await?
    } else {
        None
    };
    let existing = if existing.is_some() {
        existing
    } else {
        let mut found = discover(http, guild, parent.id, marker, bot).await?;
        if found.len() > 1 {
            return Err("Multiple private spectator threads bear this ownership marker.".into());
        }
        found.pop()
    };
    let thread = if let Some(mut thread) = existing {
        owned(&thread, guild, marker, bot)?;
        if thread.parent_id != Some(parent.id) {
            return Err("Spectator thread parent changed.".into());
        }
        if active(&thread).is_err() {
            thread = thread
                .id
                .edit_thread(
                    http.as_ref(),
                    EditThread::new()
                        .archived(false)
                        .locked(false)
                        .invitable(false),
                )
                .await
                .map_err(|e| e.to_string())?;
        }
        for id in members(http, thread.id).await? {
            if id != bot && !viewers_set.contains(&id) {
                thread
                    .id
                    .remove_thread_member(http.as_ref(), UserId::new(id))
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        thread
    } else {
        parent
            .id
            .create_thread(
                http.as_ref(),
                CreateThread::new(name)
                    .kind(ChannelType::PrivateThread)
                    .invitable(false)
                    .auto_archive_duration(AutoArchiveDuration::OneDay),
            )
            .await
            .map_err(|e| e.to_string())?
    };
    owned(&thread, guild, marker, bot)?;
    active(&thread)?;
    audit(http, guild, marker, participants, viewers, thread.id.get()).await?;
    Ok(thread.id.get())
}

pub(super) async fn audit(
    http: &Arc<Http>,
    guild: u64,
    marker: &str,
    participants: &[u64],
    viewers: &[u64],
    id: u64,
) -> Result<(), String> {
    let thread = channel(http, id)
        .await?
        .ok_or("Private spectator thread no longer exists.")?;
    let parent = parent_channel(
        http,
        guild,
        thread
            .parent_id
            .ok_or("Spectator thread has no parent.")?
            .get(),
    )
    .await?;
    let (bot, viewers) = policy(http, guild, &parent, participants, viewers).await?;
    owned(&thread, guild, marker, bot)?;
    active(&thread)?;
    check_members(&members(http, thread.id).await?, bot, &viewers)
}

pub(super) async fn delete_by_marker(
    http: &Arc<Http>,
    guild: u64,
    marker: &str,
) -> Result<(), String> {
    // Legacy records have their marker in a channel topic, handled by the caller.
    if thread_name(guild, marker).is_err() {
        return Ok(());
    }
    let bot = http
        .get_current_user()
        .await
        .map_err(|e| e.to_string())?
        .id
        .get();
    let data = http
        .get_guild(GuildId::new(guild))
        .await
        .map_err(|e| e.to_string())?;
    let member = http
        .get_member(GuildId::new(guild), UserId::new(bot))
        .await
        .map_err(|e| e.to_string())?;
    for parent in http
        .get_channels(GuildId::new(guild))
        .await
        .map_err(|e| e.to_string())?
    {
        if parent.kind != ChannelType::Text
            || !data
                .user_permissions_in(&parent, &member)
                .contains(read_permissions())
        {
            continue;
        }
        for thread in discover(http, guild, parent.id, marker, bot).await? {
            super::delete(http, guild, thread.id.get(), marker).await?;
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "runtime-test-match"))]
mod tests {
    use super::*;
    const MARKER: &str = "cama-spectator:100:619:0123456789abcdef0123456789abcdef";

    fn fixture() -> GuildChannel {
        let mut thread = GuildChannel::default();
        thread.id = ChannelId::new(500);
        thread.guild_id = GuildId::new(100);
        thread.parent_id = Some(ChannelId::new(400));
        thread.owner_id = Some(UserId::new(3));
        thread.kind = ChannelType::PrivateThread;
        thread.name = thread_name(100, MARKER).unwrap();
        thread.thread_metadata = Some(serde_json::from_value(serde_json::json!({
            "archived": false, "auto_archive_duration": 1440, "locked": false, "invitable": false
        })).unwrap());
        thread
    }

    #[test]
    fn uses_existing_thread_permissions_without_channel_or_role_management() {
        assert!(!required_permissions().intersects(
            Permissions::MANAGE_CHANNELS
                | Permissions::MANAGE_ROLES
                | Permissions::MANAGE_MESSAGES
                | Permissions::CREATE_PUBLIC_THREADS
        ));
        assert!(required_permissions().contains(
            Permissions::CREATE_PRIVATE_THREADS
                | Permissions::SEND_MESSAGES_IN_THREADS
                | Permissions::MANAGE_THREADS
        ));
        let payload = serde_json::to_value(
            CreateThread::new("spectators")
                .kind(ChannelType::PrivateThread)
                .invitable(false),
        )
        .unwrap();
        assert_eq!(payload["type"], 12);
        assert_eq!(payload["invitable"], false);
    }

    #[test]
    fn moderator_players_cannot_bypass_membership_exclusion() {
        assert!(participant_can_read(1, 1, Permissions::empty()));
        assert!(participant_can_read(9, 1, Permissions::ADMINISTRATOR));
        assert!(participant_can_read(
            9,
            1,
            Permissions::VIEW_CHANNEL | Permissions::MANAGE_THREADS
        ));
        assert!(!participant_can_read(
            9,
            1,
            Permissions::VIEW_CHANNEL | Permissions::CREATE_PRIVATE_THREADS
        ));
        assert!(!participant_can_read(9, 1, Permissions::MANAGE_THREADS));
    }

    #[test]
    fn read_only_spectators_can_watch_without_chat_or_invite_permissions() {
        let permissions = Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY;
        assert!(!permissions.intersects(
            Permissions::SEND_MESSAGES
                | Permissions::SEND_MESSAGES_IN_THREADS
                | Permissions::CREATE_PRIVATE_THREADS
                | Permissions::MANAGE_THREADS
        ));
        assert!(check_viewer_permissions(permissions, 20, 400).is_ok());
        assert!(check_members(&BTreeSet::from([3, 20]), 3, &BTreeSet::from([20])).is_ok());
        // Read-only admission does not permit a player or uninvited account.
        assert!(check_members(&BTreeSet::from([3, 20, 1]), 3, &BTreeSet::from([20])).is_err());
    }

    #[test]
    fn parent_chat_deny_does_not_block_read_only_spectator_admission() {
        let mut guild = Guild::default();
        guild.id = GuildId::new(100);
        guild.owner_id = UserId::new(999);
        let mut everyone = serenity::all::Role::default();
        everyone.id = RoleId::new(100);
        everyone.permissions = read_permissions() | Permissions::SEND_MESSAGES_IN_THREADS;
        guild.roles.insert(everyone.id, everyone);
        let mut viewer = Member::default();
        viewer.guild_id = guild.id;
        viewer.user.id = UserId::new(20);
        let mut parent = GuildChannel::default();
        parent.id = ChannelId::new(400);
        parent.guild_id = guild.id;
        parent.kind = ChannelType::Text;
        parent.permission_overwrites = vec![PermissionOverwrite {
            allow: Permissions::empty(),
            deny: Permissions::SEND_MESSAGES | Permissions::SEND_MESSAGES_IN_THREADS,
            kind: PermissionOverwriteType::Member(viewer.user.id),
        }];
        let permissions = guild.user_permissions_in(&parent, &viewer);
        assert!(!permissions.contains(Permissions::SEND_MESSAGES_IN_THREADS));
        assert!(
            check_viewer_permissions(permissions, viewer.user.id.get(), parent.id.get()).is_ok()
        );
        parent.permission_overwrites[0].deny |= Permissions::VIEW_CHANNEL;
        let permissions = guild.user_permissions_in(&parent, &viewer);
        assert!(
            check_viewer_permissions(permissions, viewer.user.id.get(), parent.id.get()).is_err()
        );
    }

    #[test]
    fn spectator_read_denials_report_the_actual_parent_user_and_missing_permission() {
        for (missing, name) in [
            (Permissions::VIEW_CHANNEL, "View Channel"),
            (Permissions::READ_MESSAGE_HISTORY, "Read Message History"),
        ] {
            let error = check_viewer_permissions(
                (read_permissions() | Permissions::SEND_MESSAGES_IN_THREADS) & !missing,
                20,
                400,
            )
            .unwrap_err();
            assert!(error.contains("Spectator 20"));
            assert!(error.contains("parent channel 400"));
            assert!(error.contains(name));
            assert!(!error.contains("SEND_MESSAGES_IN_THREADS"));
        }
    }

    #[test]
    fn ownership_and_member_invite_drift_block_delivery() {
        let thread = fixture();
        assert!(owned(&thread, 100, MARKER, 3).is_ok());
        assert!(active(&thread).is_ok());
        assert!(owned(&thread, 101, MARKER, 3).is_err());
        assert!(owned(&thread, 100, MARKER, 4).is_err());
        for kind in [ChannelType::Text, ChannelType::PublicThread] {
            let mut changed = thread.clone();
            changed.kind = kind;
            assert!(owned(&changed, 100, MARKER, 3).is_err());
        }
        let mut changed = thread.clone();
        changed.name.push('x');
        assert!(owned(&changed, 100, MARKER, 3).is_err());
        for (archived, locked, invitable) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let mut changed = thread.clone();
            let m = changed.thread_metadata.as_mut().unwrap();
            m.archived = archived;
            m.locked = locked;
            m.invitable = invitable;
            assert!(active(&changed).is_err());
        }
    }

    #[test]
    fn unexpected_members_block_delivery_but_opt_ins_can_join_later() {
        let viewers = BTreeSet::from([20, 21]);
        assert!(check_members(&BTreeSet::from([3]), 3, &viewers).is_ok());
        assert!(check_members(&BTreeSet::from([3, 20, 21]), 3, &viewers).is_ok());
        assert!(check_members(&BTreeSet::from([3, 1, 20]), 3, &viewers).is_err());
        assert!(check_members(&BTreeSet::from([3, 20, 21]), 3, &BTreeSet::from([20])).is_err());
    }
}
