//! Owned private text channels. All safety inputs come from current HTTP reads.

use super::*;
use serenity::all::{
    CreateChannel, EditChannel, PermissionOverwrite, PermissionOverwriteType, RoleId,
};

fn read_permissions() -> Permissions {
    Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY
}

fn bot_permissions() -> Permissions {
    read_permissions()
        | Permissions::SEND_MESSAGES
        | Permissions::EMBED_LINKS
        | Permissions::ATTACH_FILES
        | Permissions::MANAGE_CHANNELS
        | Permissions::MANAGE_ROLES
        | Permissions::MANAGE_MESSAGES
}

fn validate_identity(guild: u64, marker: &str) -> Result<(), String> {
    if guild == 0
        || marker.is_empty()
        || marker.len() > 1024
        || marker.chars().any(char::is_control)
    {
        return Err("Invalid spectator channel ownership identity.".into());
    }
    Ok(())
}

fn roster(
    bot: u64,
    participants: &[u64],
    viewers: &[u64],
) -> Result<(BTreeSet<u64>, BTreeSet<u64>), String> {
    let participants: BTreeSet<_> = participants.iter().copied().collect();
    if bot == 0
        || participants.is_empty()
        || participants.contains(&0)
        || participants.contains(&bot)
    {
        return Err("Spectator isolation requires a valid non-bot match roster.".into());
    }
    let viewers: BTreeSet<_> = viewers
        .iter()
        .copied()
        .filter(|id| !participants.contains(id) && *id != bot)
        .collect();
    if viewers.contains(&0) || viewers.len() > 80 || participants.len() + viewers.len() + 2 > 100 {
        return Err(
            "Spectator channels support at most 80 opted-in viewers and 100 permission overwrites."
                .into(),
        );
    }
    Ok((participants, viewers))
}

fn bypasses_overwrites(owner: u64, member: u64, permissions: Permissions) -> bool {
    owner == member || permissions.contains(Permissions::ADMINISTRATOR)
}

fn overwrites(
    guild: u64,
    bot: u64,
    participants: &BTreeSet<u64>,
    viewers: &BTreeSet<u64>,
) -> Vec<PermissionOverwrite> {
    let mut result = vec![
        PermissionOverwrite {
            allow: Permissions::empty(),
            deny: Permissions::all(),
            kind: PermissionOverwriteType::Role(RoleId::new(guild)),
        },
        PermissionOverwrite {
            allow: bot_permissions(),
            deny: Permissions::all() & !bot_permissions(),
            kind: PermissionOverwriteType::Member(UserId::new(bot)),
        },
    ];
    result.extend(participants.iter().map(|id| PermissionOverwrite {
        allow: Permissions::empty(),
        deny: Permissions::all(),
        kind: PermissionOverwriteType::Member(UserId::new(*id)),
    }));
    result.extend(viewers.iter().map(|id| PermissionOverwrite {
        allow: read_permissions(),
        deny: Permissions::all() & !read_permissions(),
        kind: PermissionOverwriteType::Member(UserId::new(*id)),
    }));
    result
}

async fn current_policy(
    http: &Arc<Http>,
    guild: u64,
    participants: &[u64],
    viewers: &[u64],
) -> Result<Vec<PermissionOverwrite>, String> {
    let bot = http
        .get_current_user()
        .await
        .map_err(|e| e.to_string())?
        .id
        .get();
    let (participants, viewers) = roster(bot, participants, viewers)?;
    let guild_data = http
        .get_guild(GuildId::new(guild))
        .await
        .map_err(|e| e.to_string())?;
    if guild_data.id.get() != guild {
        return Err("Discord returned a different guild.".into());
    }
    let everyone = guild_data
        .roles
        .get(&RoleId::new(guild))
        .ok_or("Guild everyone role could not be verified.")?
        .permissions;
    let ids: BTreeSet<_> = participants
        .iter()
        .chain(&viewers)
        .copied()
        .chain([bot])
        .collect();
    let mut requests = tokio::task::JoinSet::new();
    for id in ids {
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
        let member =
            member.map_err(|e| format!("Unable to verify spectator membership {id}: {e}"))?;
        if member.user.id.get() != id || member.guild_id.get() != guild {
            return Err("Discord returned a different guild member.".into());
        }
        let mut permissions = everyone;
        for role in &member.roles {
            permissions |= guild_data
                .roles
                .get(role)
                .ok_or("Guild member role could not be verified.")?
                .permissions;
        }
        if participants.contains(&id)
            && bypasses_overwrites(guild_data.owner_id.get(), id, permissions)
        {
            return Err("A match participant owns this server or has Administrator permission; live spectator channels cannot hide information from them.".into());
        }
        if id == bot
            && !bypasses_overwrites(guild_data.owner_id.get(), id, permissions)
            && !permissions.contains(bot_permissions())
        {
            return Err(
                "The bot lacks permissions needed to maintain a private spectator channel.".into(),
            );
        }
    }
    Ok(overwrites(guild, bot, &participants, &viewers))
}

fn owned(channel: &GuildChannel, guild: u64, marker: &str) -> Result<(), String> {
    if channel.guild_id.get() != guild
        || channel.kind != ChannelType::Text
        || channel.topic.as_deref() != Some(marker)
    {
        return Err("Channel does not match the owned spectator guild, type, and marker.".into());
    }
    Ok(())
}

type PermissionMap = BTreeMap<(u8, u64), (u64, u64)>;

fn permission_map(overwrites: &[PermissionOverwrite]) -> Result<PermissionMap, String> {
    let mut result = BTreeMap::new();
    for entry in overwrites {
        let identity = match entry.kind {
            PermissionOverwriteType::Role(id) => (0, id.get()),
            PermissionOverwriteType::Member(id) => (1, id.get()),
            _ => return Err("Unknown spectator permission overwrite type.".into()),
        };
        if result
            .insert(identity, (entry.allow.bits(), entry.deny.bits()))
            .is_some()
        {
            return Err("Duplicate spectator permission overwrites.".into());
        }
    }
    Ok(result)
}

fn check_channel(
    channel: &GuildChannel,
    guild: u64,
    marker: &str,
    expected: &[PermissionOverwrite],
) -> Result<(), String> {
    owned(channel, guild, marker)?;
    if channel.parent_id.is_some()
        || permission_map(&channel.permission_overwrites)? != permission_map(expected)?
    {
        return Err(
            "Spectator channel permissions or category changed; live delivery is blocked.".into(),
        );
    }
    Ok(())
}

fn missing(error: &serenity::Error) -> bool {
    matches!(error, serenity::Error::Http(error) if error.status_code().is_some_and(|status| status.as_u16() == 404))
}

async fn channel(http: &Http, id: u64) -> Result<Option<GuildChannel>, String> {
    if id == 0 {
        return Err("Invalid spectator channel ID.".into());
    }
    match http.get_channel(ChannelId::new(id)).await {
        Ok(Channel::Guild(channel)) if channel.id.get() == id => Ok(Some(channel)),
        Ok(Channel::Guild(_)) => Err("Discord returned a different spectator channel ID.".into()),
        Ok(_) => Err("Spectator destination must be a guild text channel.".into()),
        Err(error) if missing(&error) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) async fn ensure(
    http: &Arc<Http>,
    guild: u64,
    marker: &str,
    name: &str,
    participants: &[u64],
    viewers: &[u64],
    known: Option<u64>,
) -> Result<u64, String> {
    validate_identity(guild, marker)?;
    if !(2..=100).contains(&name.chars().count()) {
        return Err("Invalid spectator channel name.".into());
    }
    let expected = current_policy(http, guild, participants, viewers).await?;
    let known = if let Some(id) = known {
        channel(http, id).await?
    } else {
        None
    };
    if let Some(known) = &known {
        owned(known, guild, marker)?;
    }
    // Recover a successful create whose response was lost. Duplicate markers
    // are ambiguous and must never select an arbitrary channel to mutate.
    let marked: Vec<_> = http
        .get_channels(GuildId::new(guild))
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|channel| channel.topic.as_deref() == Some(marker))
        .collect();
    if marked.len() > 1 {
        return Err("Multiple channels bear this spectator ownership marker.".into());
    }
    let existing = match (known, marked.into_iter().next()) {
        (Some(known), Some(found)) if known.id != found.id => {
            return Err("Spectator channel identity is ambiguous.".into());
        }
        (Some(known), _) => Some(known),
        (None, found) => found,
    };
    let created = if let Some(mut existing) = existing {
        owned(&existing, guild, marker)?;
        if existing.parent_id.is_some() {
            return Err("Owned spectator channel moved into a category.".into());
        }
        // Viewer opt-ins may change. Replace the whole overwrite list on the
        // verified owned channel, then perform a separate fresh audit.
        if permission_map(&existing.permission_overwrites)? != permission_map(&expected)? {
            existing
                .edit(
                    http.as_ref(),
                    EditChannel::new().permissions(expected.clone()),
                )
                .await
                .map_err(|e| e.to_string())?;
        }
        existing
    } else {
        GuildId::new(guild)
            .create_channel(
                http.as_ref(),
                CreateChannel::new(name)
                    .kind(ChannelType::Text)
                    .topic(marker)
                    .permissions(expected.clone()),
            )
            .await
            .map_err(|e| e.to_string())?
    };
    check_channel(&created, guild, marker, &expected)?;
    Ok(created.id.get())
}

pub(super) async fn audit(
    http: &Arc<Http>,
    guild: u64,
    marker: &str,
    participants: &[u64],
    viewers: &[u64],
    id: u64,
) -> Result<(), String> {
    validate_identity(guild, marker)?;
    let expected = current_policy(http, guild, participants, viewers).await?;
    let channel = channel(http, id)
        .await?
        .ok_or("Spectator channel no longer exists.")?;
    check_channel(&channel, guild, marker, &expected)
}

pub(super) async fn delete_by_marker(
    http: &Arc<Http>,
    guild: u64,
    marker: &str,
) -> Result<(), String> {
    validate_identity(guild, marker)?;
    let channels = http
        .get_channels(GuildId::new(guild))
        .await
        .map_err(|error| error.to_string())?;
    for channel in channels {
        if owned(&channel, guild, marker).is_ok() {
            // Re-read ownership immediately before deletion, including when
            // several channels were created with the same recovery marker.
            delete(http, guild, channel.id.get(), marker).await?;
        }
    }
    Ok(())
}

pub(super) async fn delete(
    http: &Arc<Http>,
    guild: u64,
    id: u64,
    marker: &str,
) -> Result<(), String> {
    validate_identity(guild, marker)?;
    let Some(channel) = channel(http, id).await? else {
        return Ok(());
    };
    owned(&channel, guild, marker)?;
    match http
        .delete_channel(channel.id, Some("Remove owned Cama spectator channel"))
        .await
    {
        Ok(_) => Ok(()),
        Err(error) if missing(&error) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(all(test, feature = "runtime-test-match"))]
mod tests {
    use super::*;
    use serenity::all::Role;

    fn fixture() -> (Guild, GuildChannel, Member, Member) {
        let mut guild = Guild::default();
        guild.id = GuildId::new(100);
        guild.owner_id = UserId::new(999);
        let mut everyone = Role::default();
        everyone.id = RoleId::new(100);
        everyone.permissions = Permissions::VIEW_CHANNEL | Permissions::SEND_MESSAGES;
        guild.roles.insert(everyone.id, everyone);
        let mut role = Role::default();
        role.id = RoleId::new(101);
        role.permissions = Permissions::VIEW_CHANNEL
            | Permissions::SEND_MESSAGES
            | Permissions::CREATE_INSTANT_INVITE
            | Permissions::MANAGE_WEBHOOKS
            | Permissions::CREATE_PUBLIC_THREADS;
        guild.roles.insert(role.id, role);
        let mut player = Member::default();
        player.guild_id = guild.id;
        player.user.id = UserId::new(1);
        player.roles = vec![RoleId::new(101)];
        let mut viewer = player.clone();
        viewer.user.id = UserId::new(2);
        let mut channel = GuildChannel::default();
        channel.id = ChannelId::new(500);
        channel.guild_id = guild.id;
        channel.kind = ChannelType::Text;
        channel.topic = Some("owned-random-marker".into());
        channel.permission_overwrites =
            overwrites(100, 3, &BTreeSet::from([1]), &BTreeSet::from([2]));
        (guild, channel, player, viewer)
    }

    #[test]
    fn individual_player_deny_beats_guild_role_view_allow_and_viewers_are_read_only() {
        let (guild, channel, player, viewer) = fixture();
        assert!(
            !guild
                .user_permissions_in(&channel, &player)
                .contains(Permissions::VIEW_CHANNEL)
        );
        let permissions = guild.user_permissions_in(&channel, &viewer);
        assert!(permissions.contains(read_permissions()));
        assert!(!permissions.intersects(
            Permissions::SEND_MESSAGES
                | Permissions::CREATE_INSTANT_INVITE
                | Permissions::MANAGE_WEBHOOKS
                | Permissions::CREATE_PUBLIC_THREADS
                | Permissions::CREATE_PRIVATE_THREADS
                | Permissions::SEND_MESSAGES_IN_THREADS
        ));
        let mut uninvited = viewer;
        uninvited.user.id = UserId::new(4);
        assert!(
            !guild
                .user_permissions_in(&channel, &uninvited)
                .contains(Permissions::VIEW_CHANNEL)
        );
    }

    #[test]
    fn administrator_or_server_owner_participant_must_be_rejected() {
        assert!(bypasses_overwrites(1, 1, Permissions::empty()));
        assert!(bypasses_overwrites(9, 1, Permissions::ADMINISTRATOR));
        assert!(!bypasses_overwrites(
            9,
            1,
            Permissions::MANAGE_GUILD | Permissions::VIEW_CHANNEL
        ));
    }

    #[test]
    fn channel_identity_and_permission_drift_are_fail_closed() {
        let (_, channel, _, _) = fixture();
        let expected = channel.permission_overwrites.clone();
        assert!(check_channel(&channel, 100, "owned-random-marker", &expected).is_ok());
        assert!(owned(&channel, 200, "owned-random-marker").is_err());
        assert!(owned(&channel, 100, "different-marker").is_err());
        let mut drift = channel.clone();
        drift.permission_overwrites[0].allow |= Permissions::VIEW_CHANNEL;
        assert!(check_channel(&drift, 100, "owned-random-marker", &expected).is_err());
        let mut duplicate = channel.clone();
        duplicate
            .permission_overwrites
            .push(duplicate.permission_overwrites[0].clone());
        assert!(check_channel(&duplicate, 100, "owned-random-marker", &expected).is_err());
        let mut parent = channel.clone();
        parent.parent_id = Some(ChannelId::new(555));
        assert!(check_channel(&parent, 100, "owned-random-marker", &expected).is_err());
        let mut thread = channel;
        thread.kind = ChannelType::PrivateThread;
        assert!(owned(&thread, 100, "owned-random-marker").is_err());
    }

    #[test]
    fn spectator_roster_excludes_players_and_bounds_permission_overwrites() {
        let viewers: Vec<_> = (20..100).collect();
        let (players, viewers) = roster(3, &[1, 2], &viewers).unwrap();
        assert_eq!(overwrites(100, 3, &players, &viewers).len(), 84);
        assert!(roster(3, &[1], &(20..101).collect::<Vec<_>>()).is_err());
        assert_eq!(
            roster(3, &[1, 2], &[1, 2, 3, 4, 4]).unwrap().1,
            BTreeSet::from([4])
        );
        assert!(roster(3, &[], &[]).is_err());
        assert!(roster(3, &[3], &[]).is_err());
        assert!(roster(3, &[1], &[0]).is_err());
    }

    #[test]
    fn channel_create_payload_is_private_from_its_first_request() {
        let expected = overwrites(100, 3, &BTreeSet::from([1]), &BTreeSet::from([2]));
        let value = serde_json::to_value(
            CreateChannel::new("cama-spectators")
                .kind(ChannelType::Text)
                .topic("owned-random-marker")
                .permissions(expected),
        )
        .unwrap();
        assert_eq!(value["type"], serde_json::json!(0));
        assert_eq!(value["topic"], "owned-random-marker");
        assert_eq!(value["permission_overwrites"].as_array().unwrap().len(), 4);
        assert!(value.get("parent_id").is_none());
    }
}
