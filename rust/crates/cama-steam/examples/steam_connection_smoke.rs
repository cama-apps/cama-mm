use std::time::Duration;

use cama_steam::{DotaSteamClient, GuardConfirmation, SteamAuth, SteamAuthConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let username = std::env::var("DOTA_STEAM_USERNAME")?;
    let path = std::env::var("DOTA_STEAM_SESSION_PATH")?;
    let config = SteamAuthConfig::new(username, path).with_confirmation(GuardConfirmation::Console);
    let auth = if let Ok(password) = std::env::var("DOTA_STEAM_PASSWORD") {
        println!("Authenticating the test account; Steam Guard may request approval...");
        let bootstrap = config.clone().with_password(password);
        tokio::time::timeout(
            Duration::from_secs(180),
            SteamAuth::bootstrap_login(&bootstrap),
        )
        .await??
    } else {
        println!("Testing persisted-session authentication...");
        tokio::time::timeout(Duration::from_secs(60), SteamAuth::connect(&config)).await??
    };
    println!("Steam authenticated: Steam64={}", auth.session.steam_id);
    println!("Connecting to the Dota Game Coordinator...");
    let client = tokio::time::timeout(
        Duration::from_secs(90),
        DotaSteamClient::from_authenticated(auth),
    )
    .await??;
    println!(
        "Dota coordinator ready: account_id={}",
        client.own_account_id()
    );
    match client.snapshot().await? {
        Some(lobby) => println!(
            "Existing lobby observed: id={}; no changes requested",
            lobby.lobby_id
        ),
        None => println!("Authoritative lobby cache ready: no active lobby"),
    }
    tokio::time::sleep(Duration::from_secs(5)).await;
    client.snapshot().await?;
    println!("PASS: Steam login, GC handshake, cache hydration, and follow-up snapshot");
    Ok(())
}
