use cama_steam::{DotaSteamClient, DotaSteamEvent, LobbyConfig, SteamAuth, SteamAuthConfig, Team};
use std::{io::Read, time::Duration};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn wait_empty(client: &DotaSteamClient) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if client.snapshot().await?.is_none() {
                return Ok::<(), cama_steam::DotaSteamError>(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = SteamAuthConfig::new(
        std::env::var("DOTA_STEAM_USERNAME")?,
        std::env::var("DOTA_STEAM_SESSION_PATH")?,
    );
    let auth = tokio::time::timeout(Duration::from_secs(60), SteamAuth::connect(&config)).await??;
    let client = DotaSteamClient::from_authenticated(auth).await?;
    if client.snapshot().await?.is_some() {
        return Err("Existing lobby found; refusing to change it".into());
    }
    let own_account = client.own_account_id();
    let mut events = client.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                DotaSteamEvent::LobbyUpdated(lobby) => {
                    let own: Vec<_> = lobby
                        .members
                        .iter()
                        .filter(|member| member.account_id == own_account)
                        .map(|member| (member.team, member.slot))
                        .collect();
                    println!(
                        "EVENT: lobby update; own team/slot={own:?}; region={} mode={} pick={} cheats={} bots={} spectating={} tv={} visibility={}",
                        lobby.server_region,
                        lobby.game_mode,
                        lobby.cm_pick,
                        lobby.allow_cheats,
                        lobby.fill_with_bots,
                        lobby.allow_spectating,
                        lobby.dota_tv_delay,
                        lobby.visibility
                    );
                }
                DotaSteamEvent::LobbyCleared { .. } => println!("EVENT: lobby cleared"),
                DotaSteamEvent::TransportDisconnected { reason } => {
                    println!("EVENT: transport disconnected: {reason}")
                }
                _ => {}
            }
        }
    });
    let mut random = [0_u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    let password = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let hold_seconds = std::env::var("CAMA_SMOKE_HOLD_SECONDS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(90);
    let league_id = std::env::var("CAMA_SMOKE_LEAGUE_ID")
        .ok()
        .map(|value| value.parse::<u32>())
        .transpose()?
        .unwrap_or(0);
    let mut settings = LobbyConfig {
        game_name: "Cama PR669 Ubuntu temporary test".into(),
        league_id,
        server_region: 31,
        game_mode: 2,
        dota_tv_delay: 0,
        visibility: 2,
        pass_key: Some(password),
        allow_spectating: true,
        ..LobbyConfig::default()
    };
    println!("Creating unlisted password-protected practice lobby; no invites or launch");
    let created = client.create_lobby(&settings).await;
    let lobby = match created {
        Ok(lobby) => lobby,
        Err(error) => {
            if let Some(lobby) = client.snapshot().await?
                && lobby.game_name == settings.game_name
                && lobby.leader_steam_id == client.own_steam_id()
            {
                client.destroy_lobby(lobby.lobby_id).await?;
                wait_empty(&client).await?;
                println!("Cleaned up lobby after create validation failed");
            }
            return Err(error.into());
        }
    };
    println!("PASS: lobby created; id={}", lobby.lobby_id);
    let exercise: Result<()> = async {
        settings.game_name.push_str(" configured");
        client.configure_lobby(lobby.lobby_id, &settings).await?;
        println!("PASS: authoritative lobby configuration update");
        println!("Holding connection for {hold_seconds} seconds before requesting another update");
        tokio::time::sleep(Duration::from_secs(hold_seconds)).await;
        settings.game_name.push_str(" stable");
        client.configure_lobby(lobby.lobby_id, &settings).await?;
        println!("PASS: fresh authoritative update after {hold_seconds}-second connection hold");
        client
            .set_team_slot(lobby.lobby_id, client.own_account_id(), Team::PlayerPool, 1)
            .await?;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let current = client.snapshot().await?.ok_or("Lobby disappeared")?;
                if current.members.iter().any(|member| {
                    member.account_id == client.own_account_id() && member.team == Team::PlayerPool
                }) {
                    return Ok::<(), Box<dyn std::error::Error>>(());
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await??;
        println!("PASS: host observed in player pool");
        Ok(())
    }
    .await;
    if let Err(error) = &exercise {
        println!("LOBBY EXERCISE FAILED: {error}");
    }
    println!("Removing temporary test lobby");
    client.destroy_lobby(lobby.lobby_id).await?;
    wait_empty(&client).await?;
    println!("PASS: temporary lobby removed; authoritative cache empty");
    exercise?;
    if std::env::var_os("CAMA_SMOKE_REUSE").is_some() {
        tokio::time::sleep(Duration::from_secs(5)).await;
        settings.game_name.push_str(" recreated");
        let second = client.create_lobby(&settings).await?;
        println!("PASS: second lobby created on the same session after cleanup");
        client.destroy_lobby(second.lobby_id).await?;
        wait_empty(&client).await?;
        println!("PASS: second lobby removed; authoritative cache empty");
    }
    println!("PASS: private lobby lifecycle and connection stability");
    Ok(())
}
