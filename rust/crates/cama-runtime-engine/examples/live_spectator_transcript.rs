//! Local Valve-only capture using the production normalizer and formatter.
//! Never starts the runtime, Discord, Steam, a database, or an observer client.
use cama_domain::live_announcements::{
    AnnouncementMemory, LiveAnnouncementFrame, announcement_message_with_memory,
};
use cama_runtime_engine::dota_live::{
    DOTA_LIVE_MAX_BODY_BYTES, LiveMatchSnapshot, normalize_live_league_games,
    normalize_realtime_stats,
};
use reqwest::{Client, redirect::Policy};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::Path, time::Duration};

const BASE: &str = "https://api.steampowered.com";
fn num(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}
fn api_key(path: &str) -> Result<String, String> {
    let file = fs::read_to_string(path).map_err(|_| "cannot read local env file")?;
    let key = file
        .lines()
        .filter_map(|line| {
            let (name, value) = line.trim().trim_start_matches("export ").split_once('=')?;
            (name.trim() == "STEAM_API_KEY")
                .then(|| value.trim().trim_matches(['\'', '"']).to_owned())
        })
        .next_back()
        .ok_or("STEAM_API_KEY is absent")?;
    if key.len() != 32 || !key.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("STEAM_API_KEY is not a plain 32-character hex value".into());
    }
    Ok(key)
}
async fn fetch(
    client: &Client,
    endpoint: &str,
    key: &str,
    params: &[(&str, String)],
) -> Result<Value, String> {
    let mut response = client
        .get(format!("{BASE}/{endpoint}/v1/"))
        .query(&[("key", key)])
        .query(params)
        .send()
        .await
        .map_err(|_| "Valve request failed (URL and credentials withheld)")?;
    if !response.status().is_success() {
        return Err(format!("Valve HTTP {}", response.status().as_u16()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Valve response interrupted")?
    {
        if bytes.len().saturating_add(chunk.len()) > DOTA_LIVE_MAX_BODY_BYTES {
            return Err("Valve response exceeds capture size limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "Valve response is not JSON".into())
}
fn clock(time: i64) -> String {
    format!("{}:{:02}", time / 60, time.rem_euclid(60))
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn normalize(data: &Value, match_id: u64, server: u64) -> Option<LiveMatchSnapshot> {
    if server == 0 {
        normalize_live_league_games(data, 0, 0, match_id, now())
    } else {
        normalize_realtime_stats(data, 0, 0, match_id, now())
    }
}
fn render(
    path: &Path,
    match_id: u64,
    started: &str,
    messages: &[String],
    rows: &[String],
    failures: usize,
) -> Result<(), String> {
    let mut text = format!(
        "# Live spectator transcript — match {match_id}\n\nCaptured from Valve’s `api.steampowered.com`, starting {started}. Local output only; no Steam session or Discord messages.\n\nPoll interval: 15 seconds. Production snapshot normalizer and announcement formatter. This is an observed segment of a live game, not a reconstructed whole match. Feed delay is shown when supplied; actual event-to-feed latency is not independently measured.\n\n## Announcements\n\n"
    );
    if messages.is_empty() {
        text.push_str("Waiting for a qualifying change after the baseline.\n\n");
    }
    for message in messages {
        text.push_str(message);
        text.push_str("\n\n---\n\n");
    }
    text.push_str(&format!("## Capture evidence\n\n{failures} failed or unavailable polls. Raw responses and normalized frames are saved beside this transcript.\n\n| Received UTC | Game clock | Score R–D | Net worth R–D | Heroes with K/D | Identified buildings / destroyed | Message |\n|---|---|---|---|---|---|---|\n"));
    for row in rows {
        text.push_str(row);
        text.push('\n');
    }
    text.push_str("\nThis feed provides snapshot counters. Killer–victim pairs are shown only for intervals with one credited killer accounting for every opposing death and the score change; ambiguous intervals stay grouped. It does not provide full fight boundaries. Explicit fight recaps, Roshan/Aegis events and a final winner are not supplied by the current live adapter. A building whose destroyed entry loses its identity cannot be attributed. These omissions are retained, not filled with postgame data. Capturing one public game does not establish availability for every private or custom lobby.\n");
    fs::write(path, text).map_err(|_| "cannot write transcript".into())
}
// Re-run saved Valve responses through current production code without network.
fn replay(out: &Path) -> Result<(), String> {
    let journal =
        fs::read_to_string(out.join("frames.jsonl")).map_err(|_| "cannot read capture journal")?;
    let records: Vec<Value> = journal
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .map_err(|_| "invalid frame journal")?;
    let first = records.first().ok_or("empty frame journal")?;
    let match_id = first["frame"]["match_id"]
        .as_u64()
        .ok_or("journal match ID missing")?;
    let started = first["received_at"]
        .as_str()
        .ok_or("journal timestamp missing")?;
    let mut raw = std::collections::BTreeMap::new();
    for entry in fs::read_dir(out).map_err(|_| "cannot enumerate capture")? {
        let path = entry.map_err(|_| "cannot read capture entry")?.path();
        if path
            .file_name()
            .and_then(|v| v.to_str())
            .is_some_and(|v| v.starts_with("raw-") && v.ends_with(".json"))
        {
            let bytes = fs::read(path).map_err(|_| "cannot read raw capture")?;
            let hash: String = Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let data: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid raw JSON")?;
            raw.insert(hash, data);
        }
    }
    let mut memory = AnnouncementMemory::default();
    let mut previous: Option<LiveAnnouncementFrame> = None;
    let mut messages = Vec::new();
    let mut rows = Vec::new();
    let mut verified = fs::File::create(out.join("verified-frames.jsonl"))
        .map_err(|_| "cannot create verified journal")?;
    for record in &records {
        let digest = record["raw_sha256"]
            .as_str()
            .ok_or("missing recorded hash")?;
        let data = raw
            .get(digest)
            .ok_or("raw capture does not match recorded SHA-256")?;
        if record["frame"]["match_id"].as_u64() != Some(match_id) {
            return Err("mixed-match journal".into());
        }
        let received = record["received_at"]
            .as_str()
            .ok_or("missing receive timestamp")?;
        let is_league = data.get("result").unwrap_or(data).get("games").is_some();
        let snapshot = normalize(data, match_id, u64::from(!is_league))
            .ok_or("raw match identity mismatch")?;
        let frame = snapshot.announcement_frame.ok_or("raw frame incomplete")?;
        if let Some(map) = &snapshot.map_frame {
            let bytes = cama_runtime_engine::dota_spectator_map::render_map(map)?;
            fs::write(out.join(format!("map-{}.png", map.game_time)), bytes)
                .map_err(|_| "cannot write map preview")?;
        }
        let distinct = previous
            .as_ref()
            .is_none_or(|old| frame.game_time > old.game_time);
        let message = distinct
            .then(|| {
                announcement_message_with_memory(
                    previous.as_ref(),
                    &frame,
                    snapshot.delay_seconds,
                    &mut memory,
                )
            })
            .flatten();
        let emitted = message.is_some();
        if let Some(message) = message {
            messages.push(message);
        }
        let kd = frame
            .players
            .iter()
            .filter(|p| p.kills.is_some() && p.deaths.is_some())
            .count();
        rows.push(format!(
            "| {received} | {} | {}–{} | {}–{} | {kd}/10 | {} / {} | {} |",
            clock(frame.game_time),
            frame.radiant_score.map_or("?".into(), |v| v.to_string()),
            frame.dire_score.map_or("?".into(), |v| v.to_string()),
            frame
                .radiant_net_worth
                .map_or("?".into(), |v| v.to_string()),
            frame.dire_net_worth.map_or("?".into(), |v| v.to_string()),
            frame.buildings.len(),
            frame.buildings.iter().filter(|b| b.destroyed).count(),
            if emitted {
                "yes"
            } else if distinct {
                "quiet/baseline"
            } else {
                "cached/regressed"
            }
        ));
        writeln!(verified,"{}",json!({"received_at":received,"raw_sha256":digest,"frame":frame,"map_frame":snapshot.map_frame,"delay_seconds":snapshot.delay_seconds})).map_err(|_| "cannot write verified journal")?;
        if distinct {
            previous = Some(frame);
        }
    }
    let original =
        fs::read_to_string(out.join("transcript.md")).map_err(|_| "cannot read capture summary")?;
    let failures = original
        .lines()
        .find(|line| line.contains("failed or unavailable polls."))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .ok_or("capture failure count missing")?;
    render(
        &out.join("transcript-verified.md"),
        match_id,
        started,
        &messages,
        &rows,
        failures,
    )?;
    println!(
        "Replayed {} SHA-256-verified live frames: {} announcements, {}",
        records.len(),
        messages.len(),
        out.join("transcript-verified.md").display()
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "--replay" {
        return replay(Path::new(&args[1]));
    }
    if args.len() == 3 && args[0] == "--details" {
        let key = api_key(&args[1])?;
        let out = Path::new(&args[2]);
        let journal = fs::read_to_string(out.join("frames.jsonl"))
            .map_err(|_| "cannot read capture journal")?;
        let first: Value =
            serde_json::from_str(journal.lines().next().ok_or("empty capture journal")?)
                .map_err(|_| "invalid capture journal")?;
        let match_id = first["frame"]["match_id"]
            .as_u64()
            .ok_or("missing capture match ID")?;
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(12))
            .build()
            .map_err(|_| "HTTP client setup failed")?;
        let data = fetch(
            &client,
            "IDOTA2Match_570/GetMatchDetails",
            &key,
            &[("match_id", match_id.to_string())],
        )
        .await?;
        fs::write(
            out.join("match-details.json"),
            serde_json::to_vec_pretty(&data).map_err(|_| "cannot encode match details")?,
        )
        .map_err(|_| "cannot save match details")?;
        let result = data.get("result").unwrap_or(&data);
        if result.get("match_id").and_then(num) == Some(match_id) {
            println!(
                "Valve match details: match {match_id}, duration {:?}, radiant_win {:?}",
                result.get("duration").and_then(Value::as_i64),
                result.get("radiant_win").and_then(Value::as_bool)
            );
        } else {
            println!("Valve has not returned final match details for {match_id}");
        }
        return Ok(());
    }
    if args.len() != 3 {
        return Err("usage: live_spectator_transcript ENV_FILE OUTPUT_DIR SAMPLE_COUNT; or --details ENV_FILE CAPTURE_DIR".into());
    }
    let count: usize = args[2].parse().map_err(|_| "invalid sample count")?;
    if count != 0 && !(2..=120).contains(&count) {
        return Err("sample count must be 2..120, or 0 for Valve API discovery".into());
    }
    let key = api_key(&args[0])?;
    let client = Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(12))
        .user_agent("cama-local-spectator-validation/1")
        .build()
        .map_err(|_| "HTTP client setup failed")?;
    let out = Path::new(&args[1]);
    fs::create_dir_all(out).map_err(|_| "cannot create capture directory")?;
    if count == 0 {
        let data = fetch(&client, "ISteamWebAPIUtil/GetSupportedAPIList", &key, &[]).await?;
        fs::write(
            out.join("valve-api-list.json"),
            serde_json::to_vec_pretty(&data).unwrap(),
        )
        .map_err(|_| "cannot save Valve API list")?;
        println!("Saved Valve API list locally");
        return Ok(());
    }
    let mut candidates: Vec<(u64, u64, u64, i64)> = Vec::new();
    for partner in [0, 1] {
        let data = fetch(
            &client,
            "IDOTA2Match_570/GetTopLiveGame",
            &key,
            &[("partner", partner.to_string())],
        )
        .await?;
        fs::write(
            out.join(format!("discovery-{partner}.json")),
            serde_json::to_vec_pretty(&data).unwrap(),
        )
        .map_err(|_| "cannot save discovery")?;
        let root = data.get("result").unwrap_or(&data);
        if let Some(games) = root.get("game_list").and_then(Value::as_array) {
            for game in games {
                let Some(id) = game.get("match_id").and_then(num) else {
                    continue;
                };
                let Some(server) = game.get("server_steam_id").and_then(num) else {
                    continue;
                };
                let time = game.get("game_time").and_then(Value::as_i64).unwrap_or(-1);
                let spectators = game.get("spectators").and_then(num).unwrap_or(0);
                if (120..=2100).contains(&time) && !candidates.iter().any(|(old, ..)| *old == id) {
                    candidates.push((id, server, spectators, time));
                }
            }
        }
        if !candidates.is_empty() {
            break;
        }
    }
    candidates.sort_by_key(|(_, _, spectators, _)| std::cmp::Reverse(*spectators));
    println!("Valve discovery: {} candidate live games", candidates.len());
    let mut selected = None;
    for (match_id, server, _, _) in candidates.into_iter().take(10) {
        let result = fetch(
            &client,
            "IDOTA2MatchStats_570/GetRealtimeStats",
            &key,
            &[("server_steam_id", server.to_string())],
        )
        .await;
        let data = match result {
            Ok(data) => data,
            Err(error) => {
                println!("Candidate {match_id}: {error}");
                continue;
            }
        };
        fs::write(
            out.join(format!("probe-{match_id}.json")),
            serde_json::to_vec_pretty(&data).unwrap(),
        )
        .map_err(|_| "cannot save probe")?;
        let Some(snapshot) = normalize_realtime_stats(&data, 0, 0, match_id, now()) else {
            println!("Candidate {match_id}: missing/mismatched identity");
            continue;
        };
        if snapshot.map_frame.as_ref().is_none_or(|map| {
            map.heroes
                .iter()
                .filter(|hero| hero.x.zip(hero.y).is_some())
                .count()
                < 8
        }) {
            println!("Candidate {match_id}: no usable hero map positions");
            continue;
        }
        let Some(frame) = snapshot.announcement_frame else {
            println!("Candidate {match_id}: no complete frame");
            continue;
        };
        println!(
            "Candidate {match_id}: clock {}, {} heroes",
            frame.game_time,
            frame.players.len()
        );
        if frame.players.len() == 10 && (120..=2100).contains(&frame.game_time) {
            selected = Some((match_id, server, data));
            break;
        }
    }
    if selected.is_none() {
        println!("Realtime frames unavailable; trying Valve's independent league feed");
        let data = fetch(&client, "IDOTA2Match_570/GetLiveLeagueGames", &key, &[]).await?;
        fs::write(
            out.join("league-discovery.json"),
            serde_json::to_vec_pretty(&data).unwrap(),
        )
        .map_err(|_| "cannot save league discovery")?;
        let mut games: Vec<_> = data
            .get("result")
            .unwrap_or(&data)
            .get("games")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .collect();
        games.sort_by_key(|game| {
            let clock = game
                .pointer("/scoreboard/duration")
                .and_then(Value::as_f64)
                .unwrap_or(0.0) as i64;
            (
                (clock - 1500).unsigned_abs(),
                std::cmp::Reverse(game.get("spectators").and_then(num).unwrap_or(0)),
            )
        });
        for game in games {
            let Some(match_id) = game.get("match_id").and_then(num) else {
                continue;
            };
            let Some(snapshot) = normalize(&data, match_id, 0) else {
                continue;
            };
            if snapshot.map_frame.as_ref().is_none_or(|map| {
                map.heroes
                    .iter()
                    .filter(|hero| hero.x.zip(hero.y).is_some())
                    .count()
                    < 8
            }) {
                continue;
            }
            let Some(frame) = snapshot.announcement_frame else {
                continue;
            };
            if frame.players.len() == 10 && (120..=2100).contains(&frame.game_time) {
                selected = Some((match_id, 0, data.clone()));
                break;
            }
        }
    }
    let (match_id, server, initial) =
        selected.ok_or("no live game with a complete matching frame was available")?;
    println!("Selected live match {match_id}; capturing {count} snapshots at 15-second intervals");
    let started = chrono::Utc::now().to_rfc3339();
    let mut initial = Some(initial);
    let mut previous: Option<LiveAnnouncementFrame> = None;
    let mut memory = AnnouncementMemory::default();
    let mut messages = Vec::new();
    let mut rows = Vec::new();
    let mut failures = 0;
    let mut consecutive_missing = 0;
    let mut frames_file =
        fs::File::create(out.join("frames.jsonl")).map_err(|_| "cannot create frame journal")?;
    let mut interval = tokio::time::interval(Duration::from_secs(15));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    for sample in 0..count {
        interval.tick().await;
        let data = match initial.take() {
            Some(data) => Ok(data),
            None if server == 0 => {
                fetch(
                    &client,
                    "IDOTA2Match_570/GetLiveLeagueGames",
                    &key,
                    &[("match_id", match_id.to_string())],
                )
                .await
            }
            None => {
                fetch(
                    &client,
                    "IDOTA2MatchStats_570/GetRealtimeStats",
                    &key,
                    &[("server_steam_id", server.to_string())],
                )
                .await
            }
        };
        let received = chrono::Utc::now().to_rfc3339();
        let result = data.and_then(|data| {
            let bytes = serde_json::to_vec_pretty(&data).map_err(|_| "cannot encode raw frame")?;
            let digest: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
            fs::write(out.join(format!("raw-{sample:03}.json")), bytes).map_err(|_| "cannot save raw frame")?;
            let snapshot = normalize(&data, match_id, server).ok_or("missing or mismatched live match identity")?;
            let frame = snapshot.announcement_frame.ok_or("no complete announcement frame")?;
            writeln!(frames_file, "{}", json!({"received_at":received,"raw_sha256":digest,"frame":frame,"map_frame":snapshot.map_frame,"delay_seconds":snapshot.delay_seconds})).map_err(|_| "cannot journal frame")?;
            let distinct = previous.as_ref().is_none_or(|old| frame.game_time > old.game_time);
            let message = distinct.then(|| announcement_message_with_memory(previous.as_ref(), &frame, snapshot.delay_seconds, &mut memory)).flatten();
            let emitted = message.is_some();
            if let Some(message) = message { messages.push(message); }
            let kd = frame.players.iter().filter(|p| p.kills.is_some() && p.deaths.is_some()).count();
            rows.push(format!("| {received} | {} | {}–{} | {}–{} | {kd}/10 | {} / {} | {} |", clock(frame.game_time), frame.radiant_score.map_or("?".into(), |v| v.to_string()), frame.dire_score.map_or("?".into(), |v| v.to_string()), frame.radiant_net_worth.map_or("?".into(), |v| v.to_string()), frame.dire_net_worth.map_or("?".into(), |v| v.to_string()), frame.buildings.len(), frame.buildings.iter().filter(|b| b.destroyed).count(), if emitted { "yes" } else if distinct { "quiet/baseline" } else { "cached/regressed" }));
            println!("sample {}/{count}: match {match_id}, clock {}, score {:?}–{:?}, net worth {:?}–{:?}, {kd}/10 hero counters, {} updates", sample+1, clock(frame.game_time), frame.radiant_score, frame.dire_score, frame.radiant_net_worth, frame.dire_net_worth, messages.len());
            if distinct { previous = Some(frame); }
            Ok::<(), String>(())
        });
        if let Err(error) = result {
            failures += 1;
            consecutive_missing += 1;
            rows.push(format!(
                "| {received} | unavailable | — | — | — | — | {error} |"
            ));
            println!("sample {} unavailable: {error}", sample + 1);
        } else {
            consecutive_missing = 0;
        }
        render(
            &out.join("transcript.md"),
            match_id,
            &started,
            &messages,
            &rows,
            failures,
        )?;
        if consecutive_missing >= 3 {
            break;
        }
    }
    println!(
        "Capture complete: {} messages, {} polls, {failures} unavailable; {}",
        messages.len(),
        rows.len(),
        out.join("transcript.md").display()
    );
    Ok(())
}
