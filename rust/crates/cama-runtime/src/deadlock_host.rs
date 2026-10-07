//! Dedicated Deadlock account worker. Durable intents precede remote mutations;
//! uncertain creates/launches remain reserved for reconciliation or manual review.
use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};
use async_trait::async_trait;
use cama_db::{
    deadlock::{DeadlockMatch, DeadlockRepository},
    deadlock_betting::DeadlockBettingRepository,
    deadlock_host::{DeadlockHostJob, DeadlockHostRepository},
};
use cama_domain::deadlock::DeadlockFormat;
use cama_steam::{
    SteamAuthConfig,
    deadlock::{
        DeadlockCacheTypes, DeadlockGameMode, DeadlockPartyConfig, DeadlockSteamClient,
        DeadlockTeam, current_client_version,
    },
};
use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub struct DeadlockHostConfig {
    pub account_id: u32,
    pub auth: SteamAuthConfig,
    pub cache_types: DeadlockCacheTypes,
    pub server_region: u32,
    pub data_center_codes: Vec<u32>,
    pub ping_times: Vec<u32>,
    pub auto_record: bool,
    pub result_actor_id: Option<i64>,
}
impl DeadlockHostConfig {
    /// Credentials-only configuration for bootstrap/probe CLI commands. Cache
    /// IDs deliberately remain zero and cannot be used by the production client.
    pub fn bootstrap_from_lookup(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let account_id = lookup("DEADLOCK_STEAM_ACCOUNT_ID")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|id| *id > 0)
            .ok_or("DEADLOCK_STEAM_ACCOUNT_ID is required")?;
        let username = lookup("DEADLOCK_STEAM_USERNAME")
            .filter(|value| !value.trim().is_empty())
            .ok_or("DEADLOCK_STEAM_USERNAME is required")?;
        let mut auth = SteamAuthConfig::new(
            username,
            lookup("DEADLOCK_STEAM_SESSION_PATH")
                .unwrap_or_else(|| "data/steam/deadlock/session.json".into()),
        );
        auth.password = lookup("DEADLOCK_STEAM_PASSWORD");
        auth.shared_secret = lookup("DEADLOCK_STEAM_SHARED_SECRET");
        auth.guard_code = lookup("DEADLOCK_STEAM_GUARD_CODE");
        auth.machine_token_path = lookup("DEADLOCK_STEAM_MACHINE_TOKEN_PATH").map(PathBuf::from);
        Ok(Self {
            account_id,
            auth,
            cache_types: DeadlockCacheTypes { party: 0, lobby: 0 },
            server_region: 0,
            data_center_codes: Vec::new(),
            ping_times: Vec::new(),
            auto_record: false,
            result_actor_id: None,
        })
    }
    pub fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Option<Self>, String> {
        let enabled = lookup("DEADLOCK_STEAM_ENABLED").unwrap_or_default();
        if !matches!(enabled.as_str(), "true" | "1") {
            if !matches!(enabled.as_str(), "" | "false" | "0") {
                return Err("DEADLOCK_STEAM_ENABLED must be true or false".into());
            }
            return Ok(None);
        }
        fn required(
            lookup: &mut impl FnMut(&str) -> Option<String>,
            key: &str,
        ) -> Result<String, String> {
            lookup(key)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| format!("{key} is required when Deadlock hosting is enabled"))
        }
        fn number(
            lookup: &mut impl FnMut(&str) -> Option<String>,
            key: &str,
        ) -> Result<u32, String> {
            required(lookup, key)?
                .parse::<u32>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("{key} must be a positive integer"))
        }
        fn numbers(
            lookup: &mut impl FnMut(&str) -> Option<String>,
            key: &str,
        ) -> Result<Vec<u32>, String> {
            required(lookup, key)?
                .split(',')
                .map(|n| {
                    n.trim()
                        .parse()
                        .map_err(|_| format!("{key} must contain comma-separated integers"))
                })
                .collect()
        }
        let account_id = number(&mut lookup, "DEADLOCK_STEAM_ACCOUNT_ID")?;
        let username = required(&mut lookup, "DEADLOCK_STEAM_USERNAME")?;
        let session_path = PathBuf::from(
            lookup("DEADLOCK_STEAM_SESSION_PATH")
                .unwrap_or_else(|| "data/steam/deadlock/session.json".into()),
        );
        let mut auth = SteamAuthConfig::new(username, session_path);
        auth.password = lookup("DEADLOCK_STEAM_PASSWORD");
        auth.shared_secret = lookup("DEADLOCK_STEAM_SHARED_SECRET");
        auth.guard_code = lookup("DEADLOCK_STEAM_GUARD_CODE");
        auth.machine_token_path = lookup("DEADLOCK_STEAM_MACHINE_TOKEN_PATH").map(PathBuf::from);
        let cache_types = DeadlockCacheTypes {
            party: i32::try_from(number(&mut lookup, "DEADLOCK_STEAM_PARTY_SO_TYPE")?)
                .map_err(|_| "invalid party cache type")?,
            lobby: i32::try_from(number(&mut lookup, "DEADLOCK_STEAM_LOBBY_SO_TYPE")?)
                .map_err(|_| "invalid lobby cache type")?,
        };
        cache_types.validate().map_err(|e| e.to_string())?;
        let server_region = number(&mut lookup, "DEADLOCK_STEAM_SERVER_REGION")?;
        let data_center_codes = numbers(&mut lookup, "DEADLOCK_STEAM_DATACENTER_CODES")?;
        let ping_times = numbers(&mut lookup, "DEADLOCK_STEAM_PING_TIMES")?;
        if data_center_codes.is_empty()
            || data_center_codes.len() != ping_times.len()
            || data_center_codes.contains(&0)
        {
            return Err(
                "Deadlock datacenter codes and ping times must have matching nonempty lengths"
                    .into(),
            );
        }
        let auto_record = match lookup("DEADLOCK_STEAM_AUTO_RECORD")
            .as_deref()
            .unwrap_or("false")
        {
            "true" | "1" => true,
            "false" | "0" | "" => false,
            _ => return Err("DEADLOCK_STEAM_AUTO_RECORD must be true or false".into()),
        };
        let result_actor_id = lookup("DEADLOCK_STEAM_RESULT_ACTOR_ID")
            .map(|value| {
                value
                    .parse::<i64>()
                    .ok()
                    .filter(|id| *id > 0)
                    .ok_or_else(|| {
                        "DEADLOCK_STEAM_RESULT_ACTOR_ID must be a positive Discord bot ID"
                            .to_owned()
                    })
            })
            .transpose()?;
        if auto_record && result_actor_id.is_none() {
            return Err("Automatic Deadlock results require DEADLOCK_STEAM_RESULT_ACTOR_ID for the audit trail".into());
        }
        Ok(Some(Self {
            account_id,
            auth,
            cache_types,
            server_region,
            data_center_codes,
            ping_times,
            auto_record,
            result_actor_id,
        }))
    }
    /// Call at composition time to prohibit accidentally using the Dota account
    /// or credential files for this second game.
    pub fn validate_separate_account(
        &self,
        dota_account: Option<u32>,
        dota_session: Option<&Path>,
        dota_machine: Option<&Path>,
    ) -> Result<(), String> {
        if dota_account == Some(self.account_id)
            || dota_session.is_some_and(|p| same_path(p, &self.auth.session_path))
            || dota_machine.is_some_and(|p| same_path(p, &self.auth.default_machine_token_path()))
        {
            return Err(
                "Deadlock must use a separate Steam account, session file, and machine-token file"
                    .into(),
            );
        }
        Ok(())
    }
}
fn same_path(a: &Path, b: &Path) -> bool {
    fn normalized(p: &Path) -> PathBuf {
        let absolute = std::env::current_dir().unwrap_or_default().join(p);
        if let Ok(canonical) = absolute.canonicalize() {
            return canonical;
        }
        let mut clean = PathBuf::new();
        for component in absolute.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    clean.pop();
                }
                component => clean.push(component.as_os_str()),
            }
        }
        // Resolve the closest existing ancestor, including symlinks, even
        // when several deployment directories do not exist yet.
        let mut ancestor = clean.as_path();
        let mut suffix = Vec::new();
        loop {
            if let Ok(mut canonical) = ancestor.canonicalize() {
                for component in suffix.iter().rev() {
                    canonical.push(component);
                }
                return canonical;
            }
            let (Some(parent), Some(name)) = (ancestor.parent(), ancestor.file_name()) else {
                return clean;
            };
            suffix.push(name.to_os_string());
            ancestor = parent;
        }
    }
    normalized(a) == normalized(b)
}
#[async_trait]
trait DeadlockHostTransport: Send + Sync {
    fn own_account_id(&self) -> u32;
    async fn snapshot(
        &self,
    ) -> Result<cama_steam::deadlock::DeadlockSnapshot, cama_steam::deadlock::DeadlockSteamError>;
    async fn create_party(
        &self,
        config: &DeadlockPartyConfig,
    ) -> Result<u64, cama_steam::deadlock::DeadlockSteamError>;
    async fn move_host_to_spectator(
        &self,
        party: u64,
    ) -> Result<(), cama_steam::deadlock::DeadlockSteamError>;
    async fn set_member_team(
        &self,
        party: u64,
        account: u32,
        team: DeadlockTeam,
    ) -> Result<(), cama_steam::deadlock::DeadlockSteamError>;
    async fn set_ready(
        &self,
        party: u64,
        ready: bool,
    ) -> Result<(), cama_steam::deadlock::DeadlockSteamError>;
    async fn start_match(
        &self,
        party: u64,
        mode: DeadlockGameMode,
        roster: &[(u32, DeadlockTeam)],
    ) -> Result<(), cama_steam::deadlock::DeadlockSteamError>;
    async fn leave_party(&self, party: u64)
    -> Result<(), cama_steam::deadlock::DeadlockSteamError>;
    async fn leave_finished_lobby(
        &self,
        lobby_id: u64,
        match_id: u64,
    ) -> Result<(), cama_steam::deadlock::DeadlockSteamError>;
    async fn metadata_location(
        &self,
        match_id: u64,
    ) -> Result<
        cama_steam::deadlock::DeadlockMetadataLocation,
        cama_steam::deadlock::DeadlockSteamError,
    >;
}
#[async_trait]
impl DeadlockHostTransport for DeadlockSteamClient {
    fn own_account_id(&self) -> u32 {
        self.own_account_id()
    }
    async fn snapshot(
        &self,
    ) -> Result<cama_steam::deadlock::DeadlockSnapshot, cama_steam::deadlock::DeadlockSteamError>
    {
        self.snapshot().await
    }
    async fn create_party(
        &self,
        config: &DeadlockPartyConfig,
    ) -> Result<u64, cama_steam::deadlock::DeadlockSteamError> {
        self.create_party(config).await
    }
    async fn move_host_to_spectator(
        &self,
        party: u64,
    ) -> Result<(), cama_steam::deadlock::DeadlockSteamError> {
        self.move_host_to_spectator(party).await
    }
    async fn set_member_team(
        &self,
        party: u64,
        account: u32,
        team: DeadlockTeam,
    ) -> Result<(), cama_steam::deadlock::DeadlockSteamError> {
        self.set_member_team(party, account, team).await
    }
    async fn set_ready(
        &self,
        party: u64,
        ready: bool,
    ) -> Result<(), cama_steam::deadlock::DeadlockSteamError> {
        self.set_ready(party, ready).await
    }
    async fn start_match(
        &self,
        party: u64,
        mode: DeadlockGameMode,
        roster: &[(u32, DeadlockTeam)],
    ) -> Result<(), cama_steam::deadlock::DeadlockSteamError> {
        self.start_match(party, mode, roster).await
    }
    async fn leave_party(
        &self,
        party: u64,
    ) -> Result<(), cama_steam::deadlock::DeadlockSteamError> {
        self.leave_party(party).await
    }
    async fn leave_finished_lobby(
        &self,
        lobby_id: u64,
        match_id: u64,
    ) -> Result<(), cama_steam::deadlock::DeadlockSteamError> {
        self.leave_finished_lobby(lobby_id, match_id).await
    }
    async fn metadata_location(
        &self,
        id: u64,
    ) -> Result<
        cama_steam::deadlock::DeadlockMetadataLocation,
        cama_steam::deadlock::DeadlockSteamError,
    > {
        self.metadata_location(id).await
    }
}
#[derive(Clone)]
pub struct DeadlockHostWorker {
    path: PathBuf,
    config: DeadlockHostConfig,
    http: reqwest::Client,
    last_result_attempt: Arc<tokio::sync::Mutex<Option<tokio::time::Instant>>>,
}
impl DeadlockHostWorker {
    pub fn new(path: impl AsRef<Path>, config: DeadlockHostConfig) -> Self {
        Self {
            path: path.as_ref().to_owned(),
            config,
            http: reqwest::Client::new(),
            last_result_attempt: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }
    pub fn spec(self) -> BackgroundWorkerSpec {
        BackgroundWorkerSpec::new("deadlock_host", Arc::new(self))
    }
    async fn tick(&self, client: &dyn DeadlockHostTransport) -> Result<(), String> {
        let path = self.path.clone();
        let account = self.config.account_id;
        let Some(job) = blocking(move || DeadlockHostRepository::new(path).active(account)).await?
        else {
            return Ok(());
        };
        let path = self.path.clone();
        let guild = job.guild_id;
        let id = job.match_id;
        let Some(game) =
            blocking(move || DeadlockRepository::new(path).match_by_id(guild, id)).await?
        else {
            return self.review(&job, "Reserved local match is missing").await;
        };
        if self.config.auto_record && job.phase == "running" && game.status == "running" {
            self.try_record(client, &job, &game).await?;
            return Ok(());
        }
        let snapshot = client.snapshot().await.map_err(|e| e.to_string())?;
        if matches!(game.status.as_str(), "settled" | "aborted") {
            if snapshot
                .party
                .as_ref()
                .is_some_and(|party| job.party_id.as_deref() != Some(&party.party_id.to_string()))
            {
                return self
                    .review(
                        &job,
                        "Cleanup found an unrelated party; account remains reserved",
                    )
                    .await;
            }
            if let Some(lobby) = snapshot.lobby {
                let expected = job
                    .external_match_id
                    .as_deref()
                    .and_then(|id| id.parse::<u64>().ok());
                if expected.is_none()
                    || lobby.match_id != expected
                    || game.external_match_id.and_then(|id| u64::try_from(id).ok()) != expected
                    || lobby.mode != mode(game.format).wire_value()
                {
                    return self.review(&job,"Cleanup found an unrelated or uncorrelated game lobby; account remains reserved").await;
                }
                // Valve's declared terminal states are PostMatch=2,
                // SignedOut=3 and Abandoned=4. A local result/abort alone
                // never authorizes leaving an active or unknown live game.
                if !matches!(lobby.server_state, Some(2..=4)) {
                    return Ok(());
                }
                client
                    .leave_finished_lobby(
                        lobby.lobby_id,
                        expected.ok_or("Missing terminal match identity")?,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(()); // Removal must be observed before party cleanup.
            }
            if let Some(party) = snapshot.party {
                client
                    .leave_party(party.party_id)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(()); // Wait for authoritative removal before releasing.
            }
            self.transition(&job, "released").await?;
            return Ok(());
        }
        if game.status == "economic_setup" || job.phase == "manual_review" {
            return Ok(());
        }
        if matches!(game.status.as_str(), "gathering" | "running")
            && (snapshot.lobby.is_some()
                || snapshot
                    .party
                    .as_ref()
                    .is_some_and(|party| party.matchmaking))
        {
            self.close_market(job.guild_id, job.match_id).await?;
        }
        if matches!(
            job.phase.as_str(),
            "reserved" | "create_requested" | "gathering"
        ) && (game.publication_message_id.is_none()
            || game.publication_thread_id.is_none()
            || game.thread_message_id.is_none())
        {
            return Ok(());
        }
        let mode = mode(game.format);
        match job.phase.as_str() {
            "reserved" => {
                if snapshot.party.is_some() || snapshot.lobby.is_some() {
                    return self
                        .review(&job, "Account already has an unowned party or lobby")
                        .await;
                }
                let version = current_client_version(&self.http)
                    .await
                    .map_err(|e| e.to_string())?;
                // Version and cache preflight are read-only, before the durable intent.
                if !self.transition(&job, "create_requested").await? {
                    return Ok(());
                }
                let config = DeadlockPartyConfig {
                    mode,
                    client_version: version,
                    server_region: self.config.server_region,
                    data_center_codes: self.config.data_center_codes.clone(),
                    ping_times: self.config.ping_times.clone(),
                };
                match client.create_party(&config).await {
                    Ok(party_id) => {
                        self.store_party(&job, party_id, None).await?;
                    }
                    Err(error) => {
                        self.review(&job,&format!("Create response uncertain or rejected: {error}. Do not retry automatically.")).await?;
                    }
                }
            }
            "create_requested" => {
                // A lost create response cannot safely identify a party by name:
                // Deadlock parties carry no Cama correlation token.
                let Some(expected) = job.party_id.as_deref().and_then(|v| v.parse::<u64>().ok())
                else {
                    return self.review(&job,"Create intent has no confirmed party ID; organizer reconciliation required").await;
                };
                if let Some(party) = snapshot.party {
                    if party.party_id != expected
                        || party.mode != mode.wire_value()
                        || !party.private
                    {
                        return self
                            .review(&job, "Created party identity or format mismatch")
                            .await;
                    }
                    self.store_party(&job, party.party_id, party.join_code)
                        .await?;
                    self.transition(&job, "gathering").await?;
                }
            }
            "gathering" | "launch_requested" | "running" => {
                let expected = job
                    .party_id
                    .as_deref()
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or("Missing durable party identity")?;
                if let Some(lobby) = snapshot.lobby {
                    if job.external_match_id.is_none()
                        && snapshot
                            .party
                            .as_ref()
                            .is_none_or(|party| party.party_id != expected)
                    {
                        self.close_market(job.guild_id, job.match_id).await?;
                        return self.review(&job, "Observed an uncorrelated live lobby; no automatic match attachment").await;
                    }
                    if let Some(recorded) = job.external_match_id.as_deref()
                        && lobby.match_id.map(|id| id.to_string()).as_deref() != Some(recorded)
                    {
                        return self
                            .review(&job, "Observed external match identity changed")
                            .await;
                    }
                    if lobby.mode != mode.wire_value() {
                        return self
                            .review(&job, "Game mode changed after betting opened")
                            .await;
                    }
                    // A live lobby exists only after this account entered the
                    // reserved party; no automatic postgame/winner inference.
                    if let Some(external) = lobby.match_id {
                        self.close_market(job.guild_id, job.match_id).await?;
                        let path = self.path.clone();
                        let copy = job.clone();
                        blocking(move || {
                            DeadlockHostRepository::new(path).running(&copy, external, now())
                        })
                        .await?;
                    }
                    return Ok(());
                }
                if job.phase == "running" {
                    return Ok(());
                }
                let Some(party) = snapshot.party else {
                    self.close_market(job.guild_id, job.match_id).await?;
                    return self.review(&job,"Owned party disappeared before launch; wagers remain closed for review").await;
                };
                if party.party_id != expected || party.mode != mode.wire_value() || !party.private {
                    self.close_market(job.guild_id, job.match_id).await?;
                    return self
                        .review(
                            &job,
                            "Party identity/format changed after market publication",
                        )
                        .await;
                }
                self.store_party(&job, party.party_id, party.join_code)
                    .await?;
                if party.matchmaking {
                    self.close_market(job.guild_id, job.match_id).await?;
                    return Ok(());
                }
                let roster = expected_roster(&game)?;
                if job.phase == "launch_requested" {
                    let host_ready = party.members.iter().any(|member| {
                        member.account_id == client.own_account_id()
                            && member.spectator
                            && member.ready
                    });
                    if !host_ready || !party.roster_ready(mode, &roster) {
                        return Ok(());
                    }
                    let path = self.path.clone();
                    let copy = job.clone();
                    if blocking(move || {
                        DeadlockHostRepository::new(path).claim_explicit_start(&copy, now())
                    })
                    .await?
                        && let Err(error) = client.start_match(expected, mode, &roster).await
                    {
                        self.review(
                            &job,
                            &format!(
                                "Launch uncertain or rejected: {error}. Betting remains closed."
                            ),
                        )
                        .await?;
                    }
                    return Ok(());
                }
                // A gathering host must stay unready, including after reconnect
                // or an external readiness change, until durable launch admission.
                if party
                    .members
                    .iter()
                    .any(|member| member.account_id == client.own_account_id() && member.ready)
                {
                    client
                        .set_ready(expected, false)
                        .await
                        .map_err(|error| error.to_string())?;
                    return Ok(());
                }
                if party
                    .members
                    .iter()
                    .any(|member| member.account_id == client.own_account_id() && !member.spectator)
                {
                    client
                        .move_host_to_spectator(expected)
                        .await
                        .map_err(|e| e.to_string())?;
                    return Ok(());
                }
                for (account, team) in &roster {
                    if party.members.iter().any(|member| {
                        member.account_id == *account
                            && !member.ready
                            && member.team != Some(team.wire_value())
                    }) {
                        client
                            .set_member_team(expected, *account, *team)
                            .await
                            .map_err(|e| e.to_string())?;
                        return Ok(());
                    }
                }
                if !party.roster_ready(mode, &roster) {
                    return Ok(());
                }
                let path = self.path.clone();
                let guild = job.guild_id;
                let id = job.match_id;
                let market =
                    blocking(move || DeadlockBettingRepository::new(path).market(guild, id))
                        .await?
                        .ok_or("Deadlock market missing; launch blocked")?;
                if game
                    .thread_message_id
                    .map(discord_message_unix_seconds)
                    .is_none_or(|published| published >= market.deadline)
                {
                    return self.review(&job,"Betting market was not published before its deadline; organizer review required").await;
                }
                if market.status == "open" && now() < market.deadline {
                    return Ok(());
                }
                self.close_market(job.guild_id, job.match_id).await?;
                if !self.transition(&job, "launch_requested").await? {
                    return Ok(());
                }
                // Readying the final spectator may itself trigger allocation.
                // Both market closure and launch intent are durable already.
                if party
                    .members
                    .iter()
                    .any(|member| member.account_id == client.own_account_id() && !member.ready)
                    && let Err(error) = client.set_ready(expected, true).await
                {
                    self.review(
                        &job,
                        &format!(
                            "Ready/launch response uncertain: {error}. Betting remains closed."
                        ),
                    )
                    .await?;
                }
                // Await authoritative readiness or the resulting live lobby;
                // explicit start is independently fenced on a following tick.
            }
            _ => {}
        }
        Ok(())
    }
    async fn try_record(
        &self,
        client: &dyn DeadlockHostTransport,
        job: &DeadlockHostJob,
        game: &DeadlockMatch,
    ) -> Result<(), String> {
        {
            let mut last = self.last_result_attempt.lock().await;
            if last.is_some_and(|instant| instant.elapsed() < Duration::from_secs(60)) {
                return Ok(());
            }
            *last = Some(tokio::time::Instant::now());
        }
        let external = job
            .external_match_id
            .as_deref()
            .and_then(|id| id.parse::<u64>().ok())
            .ok_or("Running host has no external match identity")?;
        let location = match client.metadata_location(external).await {
            Ok(location) => location,
            Err(error) => {
                tracing::debug!(%error, match_id=game.match_id,"Deadlock final metadata not available yet");
                return Ok(());
            }
        };
        let roster = expected_roster(game)?;
        let result = match cama_steam::deadlock_metadata::fetch_final_result(&location,mode(game.format),roster.clone(),game.created_at).await {
            Ok(result) => result,
            Err(cama_steam::deadlock_metadata::DeadlockMetadataError::WrongMatch) => return self.review(job,"Final metadata does not match frozen roster, teams, format or external match identity").await,
            Err(error) => { tracing::debug!(%error,match_id=game.match_id,"Deadlock final metadata pending qualification or completion"); return Ok(()); }
        };
        let actor = self
            .config
            .result_actor_id
            .ok_or("Automatic results require an explicit bot audit actor")?;
        let winner = match result.winner {
            DeadlockTeam::One => 1,
            DeadlockTeam::Two => 2,
        };
        let external =
            i64::try_from(external).map_err(|_| "External match ID exceeds storage range")?;
        let evidence = serde_json::json!({"source":"valve_gc_metadata","match_id":external,"winner":winner,"format":game.format.as_str(),"start_time":result.start_time,"duration_seconds":result.duration_seconds,"players":roster,"brawl_rounds":result.brawl_rounds,"verified_at":now()}).to_string();
        let path = self.path.clone();
        let job = job.clone();
        blocking(move || {
            DeadlockHostRepository::new(&path)
                .record_evidence(&job, &evidence, now())
                .map_err(|e| e.to_string())?;
            DeadlockRepository::new(&path)
                .record_result(
                    job.guild_id,
                    job.match_id,
                    winner,
                    actor,
                    Some(external),
                    now(),
                )
                .map_err(|e| e.to_string())?;
            DeadlockBettingRepository::new(path)
                .settle_recorded(job.guild_id, job.match_id, now())
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
    }
    async fn transition(&self, job: &DeadlockHostJob, phase: &'static str) -> Result<bool, String> {
        let path = self.path.clone();
        let job = job.clone();
        blocking(move || DeadlockHostRepository::new(path).transition(&job, phase, now())).await
    }
    async fn store_party(
        &self,
        job: &DeadlockHostJob,
        party: u64,
        code: Option<u64>,
    ) -> Result<(), String> {
        let path = self.path.clone();
        let job = job.clone();
        blocking(move || DeadlockHostRepository::new(path).party(&job, party, code, now())).await
    }
    async fn close_market(&self, guild: i64, id: i64) -> Result<(), String> {
        let path = self.path.clone();
        blocking(move || {
            DeadlockBettingRepository::new(path)
                .close_market(guild, id, now())
                .map(|_| ())
        })
        .await
    }
    async fn review(&self, job: &DeadlockHostJob, reason: &str) -> Result<(), String> {
        let path = self.path.clone();
        let job = job.clone();
        let reason = reason.to_owned();
        blocking(move || {
            let economy = DeadlockBettingRepository::new(&path);
            if economy
                .market(job.guild_id, job.match_id)
                .map_err(|e| e.to_string())?
                .is_some_and(|market| market.status == "open")
            {
                economy
                    .close_market(job.guild_id, job.match_id, now())
                    .map_err(|e| e.to_string())?;
            }
            DeadlockHostRepository::new(path)
                .manual_review(&job, &reason, now())
                .map_err(|e| e.to_string())
        })
        .await
    }
}
fn mode(format: DeadlockFormat) -> DeadlockGameMode {
    match format {
        DeadlockFormat::StreetBrawl => DeadlockGameMode::StreetBrawl,
        DeadlockFormat::Standard => DeadlockGameMode::Standard,
    }
}
fn expected_roster(game: &DeadlockMatch) -> Result<Vec<(u32, DeadlockTeam)>, String> {
    game.team1
        .iter()
        .map(|p| (p, DeadlockTeam::One))
        .chain(game.team2.iter().map(|p| (p, DeadlockTeam::Two)))
        .map(|(p, team)| {
            u32::try_from(p.steam_id)
                .ok()
                .filter(|id| *id > 0)
                .map(|id| (id, team))
                .ok_or_else(|| "Invalid Steam32 identity in frozen Deadlock roster".into())
        })
        .collect()
}
fn discord_message_unix_seconds(message_id: i64) -> i64 {
    ((message_id >> 22) + 1_420_070_400_000) / 1000
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
async fn blocking<T: Send + 'static, E: std::fmt::Display + Send + 'static>(
    call: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(call)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}
fn acquire_account_lock(config: &DeadlockHostConfig) -> Result<File, String> {
    let path = config.auth.session_path.with_extension("host.lock");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.try_lock_exclusive()
        .map_err(|_| "Another local process already owns this Deadlock Steam session".to_owned())?;
    Ok(file)
}
/// Read safe welcome-cache diagnostics using the persisted dedicated session.
/// This never creates a party or sends a start command.
pub async fn probe_steam_cache(
    config: &DeadlockHostConfig,
) -> Result<Vec<cama_steam::deadlock::DeadlockCacheProbe>, String> {
    let config_copy = config.clone();
    let _guard = blocking(move || acquire_account_lock(&config_copy)).await?;
    let authenticated = cama_steam::SteamAuth::connect(&config.auth)
        .await
        .map_err(|e| e.to_string())?;
    if authenticated.session.steam_id & 0xffff_ffff != u64::from(config.account_id) {
        return Err(
            "Authenticated account differs from configured dedicated Deadlock account".into(),
        );
    }
    cama_steam::deadlock::probe_cache(authenticated)
        .await
        .map_err(|e| e.to_string())
}
/// Explicit first login, with private refresh/machine-token persistence.
/// It does not create a lobby or enable automatic settlement.
pub async fn bootstrap_steam_login(
    config: &DeadlockHostConfig,
) -> Result<cama_steam::SteamSession, String> {
    let config_copy = config.clone();
    let _account_guard = blocking(move || acquire_account_lock(&config_copy)).await?;
    let auth = config
        .auth
        .clone()
        .with_confirmation(cama_steam::GuardConfirmation::Console);
    let authenticated = cama_steam::SteamAuth::bootstrap_login(&auth)
        .await
        .map_err(|e| e.to_string())?;
    let actual = authenticated.session.steam_id & 0xffff_ffff;
    if actual != u64::from(config.account_id) {
        return Err(
            "Authenticated account differs from configured dedicated Deadlock account".into(),
        );
    }
    Ok(authenticated.session)
}
#[async_trait]
impl BackgroundWorker for DeadlockHostWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        let config = self.config.clone();
        let _account_guard = blocking(move || acquire_account_lock(&config)).await?;
        let client = tokio::select! {()=context.cancelled()=>return Ok(()), result=DeadlockSteamClient::connect_for_account(&self.config.auth,self.config.cache_types,self.config.account_id)=>result.map_err(|e|e.to_string())?};
        if client.own_account_id() != self.config.account_id {
            return Err(
                "Authenticated Deadlock account differs from configured dedicated account".into(),
            );
        }
        let mut events = client.subscribe();
        loop {
            tokio::select! {()=context.cancelled()=>return Ok(()),result=self.tick(&client)=>result?};
            tokio::select! {
                ()=context.cancelled()=>return Ok(()),
                ()=tokio::time::sleep(Duration::from_secs(5))=>{},
                event=events.recv()=>{if matches!(event,Err(tokio::sync::broadcast::error::RecvError::Closed)){return Err("Deadlock coordinator event stream closed".into());}},
            }
        }
    }
}

#[cfg(all(test, feature = "runtime-test-match"))]
mod tests {
    use super::*;
    use std::collections::HashMap;
    fn values() -> HashMap<String, String> {
        [
            ("DEADLOCK_STEAM_ENABLED", "true"),
            ("DEADLOCK_STEAM_ACCOUNT_ID", "42"),
            ("DEADLOCK_STEAM_USERNAME", "fixture-host"),
            ("DEADLOCK_STEAM_PARTY_SO_TYPE", "3001"),
            ("DEADLOCK_STEAM_LOBBY_SO_TYPE", "3002"),
            ("DEADLOCK_STEAM_SERVER_REGION", "1"),
            ("DEADLOCK_STEAM_DATACENTER_CODES", "123,456"),
            ("DEADLOCK_STEAM_PING_TIMES", "20,70"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }
    #[test]
    fn hosting_disabled_does_not_require_credentials() {
        assert!(DeadlockHostConfig::from_lookup(|_| None).unwrap().is_none());
    }
    #[test]
    fn dedicated_account_defaults_paths_and_redacts_password() {
        let mut env = values();
        env.insert("DEADLOCK_STEAM_PASSWORD".into(), "fixture-secret".into());
        let config = DeadlockHostConfig::from_lookup(|k| env.get(k).cloned())
            .unwrap()
            .unwrap();
        assert_eq!(
            config.auth.session_path,
            PathBuf::from("data/steam/deadlock/session.json")
        );
        assert_eq!(
            config.auth.default_machine_token_path(),
            PathBuf::from("data/steam/deadlock/machine_tokens.json")
        );
        assert!(!format!("{config:?}").contains("fixture-secret"));
        assert!(
            config
                .validate_separate_account(Some(42), None, None)
                .is_err()
        );
        assert!(
            config
                .validate_separate_account(
                    Some(99),
                    Some(Path::new("data/steam/dota/session.json")),
                    None
                )
                .is_ok()
        );
        assert!(
            config
                .validate_separate_account(None, Some(&config.auth.session_path), None)
                .is_err()
        );
    }
    #[test]
    fn missing_cache_qualification_or_mismatched_ping_vectors_fail_closed() {
        let mut env = values();
        env.remove("DEADLOCK_STEAM_PARTY_SO_TYPE");
        assert!(DeadlockHostConfig::from_lookup(|k| env.get(k).cloned()).is_err());
        let mut env = values();
        env.insert("DEADLOCK_STEAM_PING_TIMES".into(), "10".into());
        assert!(DeadlockHostConfig::from_lookup(|k| env.get(k).cloned()).is_err());
    }
    struct FakeHost {
        path: PathBuf,
        snapshot: cama_steam::deadlock::DeadlockSnapshot,
        starts: std::sync::atomic::AtomicUsize,
        lose_launch_reply: bool,
        lobby_leaves: std::sync::atomic::AtomicUsize,
        party_leaves: std::sync::atomic::AtomicUsize,
        ready_calls: std::sync::atomic::AtomicUsize,
        lose_ready_reply: bool,
    }
    #[async_trait]
    impl DeadlockHostTransport for FakeHost {
        fn own_account_id(&self) -> u32 {
            42
        }
        async fn snapshot(
            &self,
        ) -> Result<cama_steam::deadlock::DeadlockSnapshot, cama_steam::deadlock::DeadlockSteamError>
        {
            Ok(self.snapshot.clone())
        }
        async fn create_party(
            &self,
            _: &DeadlockPartyConfig,
        ) -> Result<u64, cama_steam::deadlock::DeadlockSteamError> {
            panic!("a gathering job must never create another party")
        }
        async fn move_host_to_spectator(
            &self,
            _: u64,
        ) -> Result<(), cama_steam::deadlock::DeadlockSteamError> {
            panic!("fixture host already spectator")
        }
        async fn set_member_team(
            &self,
            _: u64,
            _: u32,
            _: DeadlockTeam,
        ) -> Result<(), cama_steam::deadlock::DeadlockSteamError> {
            panic!("ready players must not be reassigned")
        }
        async fn set_ready(
            &self,
            _: u64,
            ready: bool,
        ) -> Result<(), cama_steam::deadlock::DeadlockSteamError> {
            if ready {
                assert_eq!(
                    DeadlockBettingRepository::new(&self.path)
                        .market(10, 1)
                        .unwrap()
                        .unwrap()
                        .status,
                    "closed"
                );
                assert_eq!(
                    DeadlockHostRepository::new(&self.path)
                        .active(42)
                        .unwrap()
                        .unwrap()
                        .phase,
                    "launch_requested"
                );
            }
            self.ready_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.lose_ready_reply {
                Err(cama_steam::deadlock::DeadlockSteamError::Timeout)
            } else {
                Ok(())
            }
        }
        async fn start_match(
            &self,
            _: u64,
            _: DeadlockGameMode,
            _: &[(u32, DeadlockTeam)],
        ) -> Result<(), cama_steam::deadlock::DeadlockSteamError> {
            assert_eq!(
                DeadlockBettingRepository::new(&self.path)
                    .market(10, 1)
                    .unwrap()
                    .unwrap()
                    .status,
                "closed"
            );
            assert_eq!(
                DeadlockHostRepository::new(&self.path)
                    .active(42)
                    .unwrap()
                    .unwrap()
                    .phase,
                "launch_requested"
            );
            self.starts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.lose_launch_reply {
                Err(cama_steam::deadlock::DeadlockSteamError::Timeout)
            } else {
                Ok(())
            }
        }
        async fn leave_party(
            &self,
            _: u64,
        ) -> Result<(), cama_steam::deadlock::DeadlockSteamError> {
            self.party_leaves
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        async fn leave_finished_lobby(
            &self,
            _: u64,
            _: u64,
        ) -> Result<(), cama_steam::deadlock::DeadlockSteamError> {
            self.lobby_leaves
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        async fn metadata_location(
            &self,
            _: u64,
        ) -> Result<
            cama_steam::deadlock::DeadlockMetadataLocation,
            cama_steam::deadlock::DeadlockSteamError,
        > {
            Err(cama_steam::deadlock::DeadlockSteamError::MissingMetadata)
        }
    }
    fn fixture(
        lose_launch_reply: bool,
        future_window: bool,
    ) -> (tempfile::NamedTempFile, DeadlockHostWorker, FakeHost) {
        let db = tempfile::NamedTempFile::new().unwrap();
        crate::test_support::initialize_test_database(db.path()).unwrap();
        let connection = cama_db::open_runtime_connection(db.path()).unwrap();
        let players: Vec<_> = (1..=8)
            .map(|id| cama_domain::deadlock::DeadlockPlayer {
                discord_id: id,
                steam_id: id,
                mu: 25.0,
                sigma: 8.0,
            })
            .collect();
        let teams = cama_domain::deadlock::DeadlockTeams {
            team1: players[..4].to_vec(),
            team2: players[4..].to_vec(),
        };
        for id in 1..=8 {
            connection.execute("INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance) VALUES(?1,10,'Fixture',0)",[id]).unwrap();
        }
        connection.execute("INSERT INTO deadlock_matches(match_id,guild_id,format,status,roster_json,created_by,created_at,updated_at) VALUES(1,10,'street_brawl','economic_setup',?1,1,100,100)",[serde_json::to_string(&teams).unwrap()]).unwrap();
        let published_snowflake = ((now() - 10) * 1000 - 1_420_070_400_000) << 22;
        connection.execute("UPDATE deadlock_matches SET publication_channel_id=99,publication_message_id=?1,publication_thread_id=?1,thread_message_id=?1 WHERE match_id=1",[published_snowflake]).unwrap();
        let repo = DeadlockHostRepository::new(db.path());
        repo.reserve(10, 1, 42, 100).unwrap();
        let reserved = repo.active(42).unwrap().unwrap();
        repo.transition(&reserved, "create_requested", 100).unwrap();
        let created = repo.active(42).unwrap().unwrap();
        repo.party(&created, 999, Some(123), 100).unwrap();
        repo.transition(&created, "gathering", 100).unwrap();
        let terms = cama_db::deadlock_betting::DeadlockMarketTerms {
            betting_window_seconds: if future_window { 600 } else { 1 },
            ..Default::default()
        };
        DeadlockBettingRepository::new(db.path())
            .open_market(10, 1, &terms, if future_window { now() } else { now() - 2 })
            .unwrap();
        let config = DeadlockHostConfig::from_lookup(|key| values().get(key).cloned())
            .unwrap()
            .unwrap();
        let worker = DeadlockHostWorker::new(db.path(), config);
        let mut members: Vec<_> = players
            .iter()
            .map(|p| cama_steam::deadlock::DeadlockPartyMember {
                account_id: p.steam_id as u32,
                ready: true,
                spectator: false,
                team: Some(if p.steam_id <= 4 { 0 } else { 1 }),
                admin: false,
            })
            .collect();
        members.push(cama_steam::deadlock::DeadlockPartyMember {
            account_id: 42,
            ready: false,
            spectator: true,
            team: Some(16),
            admin: true,
        });
        let fake = FakeHost {
            path: db.path().to_owned(),
            snapshot: cama_steam::deadlock::DeadlockSnapshot {
                party: Some(cama_steam::deadlock::DeadlockPartySnapshot {
                    party_id: 999,
                    join_code: Some(123),
                    mode: 4,
                    private: true,
                    members,
                    matchmaking: false,
                }),
                lobby: None,
            },
            starts: std::sync::atomic::AtomicUsize::new(0),
            lose_launch_reply,
            lobby_leaves: std::sync::atomic::AtomicUsize::new(0),
            party_leaves: std::sync::atomic::AtomicUsize::new(0),
            ready_calls: std::sync::atomic::AtomicUsize::new(0),
            lose_ready_reply: false,
        };
        (db, worker, fake)
    }
    #[tokio::test]
    async fn betting_closure_and_launch_intent_are_durable_before_remote_launch() {
        let (_db, worker, mut fake) = fixture(false, false);
        worker.tick(&fake).await.unwrap();
        assert_eq!(
            fake.ready_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(fake.starts.load(std::sync::atomic::Ordering::SeqCst), 0);
        fake.snapshot
            .party
            .as_mut()
            .unwrap()
            .members
            .last_mut()
            .unwrap()
            .ready = true;
        worker.tick(&fake).await.unwrap();
        worker.tick(&fake).await.unwrap();
        assert_eq!(fake.starts.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn lost_launch_reply_never_reopens_market_or_retries_launch() {
        let (db, worker, mut fake) = fixture(true, false);
        worker.tick(&fake).await.unwrap();
        fake.snapshot
            .party
            .as_mut()
            .unwrap()
            .members
            .last_mut()
            .unwrap()
            .ready = true;
        worker.tick(&fake).await.unwrap();
        worker.tick(&fake).await.unwrap();
        assert_eq!(fake.starts.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            DeadlockHostRepository::new(db.path())
                .active(42)
                .unwrap()
                .unwrap()
                .phase,
            "manual_review"
        );
        assert_eq!(
            DeadlockBettingRepository::new(db.path())
                .market(10, 1)
                .unwrap()
                .unwrap()
                .status,
            "closed"
        );
    }
    #[tokio::test]
    async fn ready_players_still_receive_advertised_betting_window() {
        let (db, worker, fake) = fixture(false, true);
        worker.tick(&fake).await.unwrap();
        assert_eq!(fake.starts.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            fake.ready_calls.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            DeadlockBettingRepository::new(db.path())
                .market(10, 1)
                .unwrap()
                .unwrap()
                .status,
            "open"
        );
    }
    #[tokio::test]
    async fn wrong_ready_team_cannot_launch() {
        let (_db, worker, mut fake) = fixture(false, false);
        fake.snapshot.party.as_mut().unwrap().members[0].team = Some(1);
        worker.tick(&fake).await.unwrap();
        assert_eq!(fake.starts.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn missing_publication_cannot_launch_and_early_manual_start_closes_market() {
        let (db, worker, mut fake) = fixture(false, true);
        cama_db::open_runtime_connection(db.path())
            .unwrap()
            .execute(
                "UPDATE deadlock_matches SET thread_message_id=NULL WHERE match_id=1",
                [],
            )
            .unwrap();
        worker.tick(&fake).await.unwrap();
        assert_eq!(fake.starts.load(std::sync::atomic::Ordering::SeqCst), 0);
        let published = ((now() - 10) * 1000 - 1_420_070_400_000) << 22;
        cama_db::open_runtime_connection(db.path())
            .unwrap()
            .execute(
                "UPDATE deadlock_matches SET thread_message_id=?1 WHERE match_id=1",
                [published],
            )
            .unwrap();
        fake.snapshot.party.as_mut().unwrap().matchmaking = true;
        worker.tick(&fake).await.unwrap();
        assert_eq!(
            DeadlockBettingRepository::new(db.path())
                .market(10, 1)
                .unwrap()
                .unwrap()
                .status,
            "closed"
        );
    }
    #[tokio::test]
    async fn publication_after_betting_deadline_requires_review() {
        let (db, worker, fake) = fixture(false, false);
        let published = (now() * 1000 - 1_420_070_400_000) << 22;
        cama_db::open_runtime_connection(db.path())
            .unwrap()
            .execute(
                "UPDATE deadlock_matches SET thread_message_id=?1 WHERE match_id=1",
                [published],
            )
            .unwrap();
        worker.tick(&fake).await.unwrap();
        assert_eq!(fake.starts.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            DeadlockHostRepository::new(db.path())
                .active(42)
                .unwrap()
                .unwrap()
                .phase,
            "manual_review"
        );
    }
    #[test]
    fn bootstrap_does_not_require_unqualified_cache_ids_or_host_enablement() {
        let config = DeadlockHostConfig::bootstrap_from_lookup(|key| match key {
            "DEADLOCK_STEAM_ACCOUNT_ID" => Some("42".into()),
            "DEADLOCK_STEAM_USERNAME" => Some("Fixture".into()),
            _ => None,
        })
        .unwrap();
        assert!(config.cache_types.validate().is_err());
        assert!(!config.auto_record);
        assert_eq!(config.account_id, 42);
    }
    fn terminal(db: &Path, fake: &mut FakeHost, status: &str, state: Option<i32>) {
        let connection = cama_db::open_runtime_connection(db).unwrap();
        connection
            .execute(
                "UPDATE deadlock_matches SET status=?1,external_match_id=12345 WHERE match_id=1",
                [status],
            )
            .unwrap();
        connection.execute("UPDATE deadlock_host_jobs SET phase='running',external_match_id='12345' WHERE match_id=1",[]).unwrap();
        connection
            .execute(
                "UPDATE deadlock_betting_markets SET status=?1 WHERE match_id=1",
                [if status == "settled" {
                    "settled"
                } else {
                    "refunded"
                }],
            )
            .unwrap();
        fake.snapshot.lobby = Some(cama_steam::deadlock::DeadlockLobbySnapshot {
            lobby_id: 5678,
            match_id: Some(12345),
            mode: 4,
            server_state: state,
        });
    }
    #[tokio::test]
    async fn settled_terminal_lobby_leaves_then_party_then_releases_after_observation() {
        let (db, worker, mut fake) = fixture(false, false);
        terminal(db.path(), &mut fake, "settled", Some(2));
        worker.tick(&fake).await.unwrap();
        assert_eq!(
            fake.lobby_leaves.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            fake.party_leaves.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert!(
            DeadlockHostRepository::new(db.path())
                .active(42)
                .unwrap()
                .is_some()
        );
        fake.snapshot.lobby = None;
        worker.tick(&fake).await.unwrap();
        assert_eq!(
            fake.party_leaves.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(
            DeadlockHostRepository::new(db.path())
                .active(42)
                .unwrap()
                .is_some()
        );
        fake.snapshot.party = None;
        worker.tick(&fake).await.unwrap();
        assert!(
            DeadlockHostRepository::new(db.path())
                .active(42)
                .unwrap()
                .is_none()
        );
    }
    #[tokio::test]
    async fn terminal_cleanup_retains_active_unknown_and_unrelated_lobbies() {
        for state in [None, Some(0), Some(1), Some(99)] {
            let (db, worker, mut fake) = fixture(false, false);
            terminal(db.path(), &mut fake, "aborted", state);
            worker.tick(&fake).await.unwrap();
            assert_eq!(
                fake.lobby_leaves.load(std::sync::atomic::Ordering::SeqCst),
                0
            );
            assert_eq!(
                fake.party_leaves.load(std::sync::atomic::Ordering::SeqCst),
                0
            );
            assert!(
                DeadlockHostRepository::new(db.path())
                    .active(42)
                    .unwrap()
                    .is_some()
            );
        }
        let (db, worker, mut fake) = fixture(false, false);
        terminal(db.path(), &mut fake, "settled", Some(2));
        fake.snapshot.lobby.as_mut().unwrap().match_id = Some(98765);
        worker.tick(&fake).await.unwrap();
        assert_eq!(
            fake.lobby_leaves.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            DeadlockHostRepository::new(db.path())
                .active(42)
                .unwrap()
                .unwrap()
                .phase,
            "manual_review"
        );
    }
    #[tokio::test]
    async fn aborted_manual_review_recovers_only_owned_party_when_no_game_remains() {
        let (db, worker, mut fake) = fixture(false, false);
        terminal(db.path(), &mut fake, "aborted", Some(4));
        let connection = cama_db::open_runtime_connection(db.path()).unwrap();
        connection
            .execute(
                "UPDATE deadlock_host_jobs SET phase='manual_review' WHERE match_id=1",
                [],
            )
            .unwrap();
        worker.tick(&fake).await.unwrap();
        assert_eq!(
            fake.lobby_leaves.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        fake.snapshot.lobby = None;
        worker.tick(&fake).await.unwrap();
        assert_eq!(
            fake.party_leaves.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        fake.snapshot.party = None;
        worker.tick(&fake).await.unwrap();
        assert!(
            DeadlockHostRepository::new(db.path())
                .active(42)
                .unwrap()
                .is_none()
        );
    }
    #[tokio::test]
    async fn lost_ready_reply_keeps_closed_intent_without_retrying_ready_or_start() {
        let (db, worker, mut fake) = fixture(false, false);
        fake.lose_ready_reply = true;
        worker.tick(&fake).await.unwrap();
        worker.tick(&fake).await.unwrap();
        assert_eq!(
            fake.ready_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(fake.starts.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            DeadlockHostRepository::new(db.path())
                .active(42)
                .unwrap()
                .unwrap()
                .phase,
            "manual_review"
        );
        assert_eq!(
            DeadlockBettingRepository::new(db.path())
                .market(10, 1)
                .unwrap()
                .unwrap()
                .status,
            "closed"
        );
    }
    #[tokio::test]
    async fn readiness_auto_start_observation_does_not_send_explicit_start() {
        let (db, worker, mut fake) = fixture(false, false);
        worker.tick(&fake).await.unwrap();
        assert_eq!(
            fake.ready_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        fake.snapshot.lobby = Some(cama_steam::deadlock::DeadlockLobbySnapshot {
            lobby_id: 5678,
            match_id: Some(12345),
            mode: 4,
            server_state: Some(1),
        });
        worker.tick(&fake).await.unwrap();
        assert_eq!(fake.starts.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            DeadlockHostRepository::new(db.path())
                .active(42)
                .unwrap()
                .unwrap()
                .phase,
            "running"
        );
    }
    #[test]
    fn nonexistent_session_aliases_are_normalized_before_separation_checks() {
        assert!(same_path(
            Path::new("./data/steam/../steam/session.json"),
            Path::new("data/steam/session.json")
        ));
        assert!(!same_path(
            Path::new("data/steam/deadlock/session.json"),
            Path::new("data/steam/dota/session.json")
        ));
    }
    #[cfg(unix)]
    #[test]
    fn missing_session_descendants_resolve_existing_symlink_ancestors() {
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        assert!(same_path(
            &real.join("missing/deeper/session.json"),
            &alias.join("missing/deeper/session.json")
        ));
    }
}
