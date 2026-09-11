//! Read-only GC metadata-coordinate probe. Uses only an existing saved session.
use std::io::Write;
use std::time::Duration;

use cama_steam::{DotaSteamClient, SteamAuthConfig};

#[cfg(unix)]
fn private_output(path: &str) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_output(_path: &str) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::other(
        "run this probe on the isolated Linux test host",
    ))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ids = std::env::args()
        .skip(1)
        .map(|id| id.parse::<u64>())
        .collect::<Result<Vec<_>, _>>()?;
    if ids.is_empty() || ids.len() > 20 || ids.contains(&0) {
        return Err("supply between one and twenty positive Valve match IDs".into());
    }
    let username = std::env::var("DOTA_STEAM_USERNAME")?;
    let session = std::env::var("DOTA_STEAM_SESSION_PATH")?;
    let path = std::env::var("DOTA_METADATA_PROBE_OUTPUT")?;
    let mut output = private_output(&path)?;
    // Password and Guard environment variables are deliberately never read.
    let config = SteamAuthConfig::new(username, session);
    let client =
        tokio::time::timeout(Duration::from_secs(120), DotaSteamClient::connect(&config)).await??;
    println!("Saved-session Steam and Dota GC connection ready");
    let mut locations = Vec::new();
    for match_id in ids {
        match client.match_metadata_location(match_id).await {
            Ok(location) => {
                println!(
                    "match_id={match_id} cluster_present={} salt_present={} key_present={}",
                    location.cluster.is_some(),
                    location.replay_salt.is_some(),
                    location.private_metadata_key().is_some(),
                );
                locations.push(serde_json::json!({
                    "match_id": location.match_id,
                    "duration": location.duration,
                    "pre_game_duration": location.pre_game_duration,
                    "start_time": location.start_time,
                    "radiant_win": location.radiant_win,
                    "cluster": location.cluster,
                    "replay_salt": location.replay_salt,
                    "private_metadata_key": location.private_metadata_key(),
                }));
            }
            Err(error) => eprintln!("match_id={match_id} request failed: {error}"),
        }
    }
    serde_json::to_writer(&mut output, &locations)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    println!(
        "Probe complete: {} coordinate records saved privately",
        locations.len()
    );
    Ok(())
}
