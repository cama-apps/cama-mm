//! Initial skill imports. Provider failures produce an explicit provisional prior.
//! Imported values never overwrite an established local rating.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::application_config::Secret;

const STEAM_ID_BASE: u64 = 76_561_197_960_265_728;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// Fixed, deliberately weak initial prior, not a calibrated badge-to-skill model.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImportedRating {
    pub mu: f64,
    pub sigma: f64,
    pub source: String,
    pub raw_value: Option<f64>,
    pub provenance: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImportedRatings {
    pub standard: ImportedRating,
    pub brawl: ImportedRating,
}

impl ImportedRatings {
    pub fn provisional(reason: &str) -> Self {
        let prior = ImportedRating {
            mu: 25.0,
            sigma: 25.0 / 3.0,
            source: "provisional-v1".to_owned(),
            raw_value: None,
            provenance: json!({"reason": reason, "transform": "weak-prior-v1"}),
        };
        Self {
            standard: prior.clone(),
            brawl: prior,
        }
    }

    fn from_standard(standard: ImportedRating) -> Self {
        let brawl = ImportedRating {
            mu: 25.0 + (standard.mu - 25.0) * 0.25,
            sigma: 25.0 / 3.0,
            source: "standard-weak-brawl-prior-v1".to_owned(),
            raw_value: standard.raw_value,
            provenance: json!({"source": standard.source, "source_provenance": standard.provenance,
                "target_mode": "street_brawl", "shrinkage": 0.25, "provisional": true}),
        };
        Self { standard, brawl }
    }
}

/// Accept an account number or an individual public-universe SteamID64.
pub fn steam_account_id(input: &str) -> Result<u32, String> {
    let id: u64 = input
        .trim()
        .parse()
        .map_err(|_| "Enter a numeric Steam account ID or SteamID64.".to_owned())?;
    let account = if id > u64::from(u32::MAX) {
        id.checked_sub(STEAM_ID_BASE)
            .ok_or_else(|| "Invalid SteamID64.".to_owned())?
    } else {
        id
    };
    u32::try_from(account)
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| "Steam ID must identify an individual public Steam account.".to_owned())
}

#[derive(Clone)]
pub struct DeadlockRatingClient {
    client: reqwest::Client,
    api_key: Option<Secret>,
    cache: Arc<Mutex<BTreeMap<u32, (Instant, ImportedRatings)>>>,
}

impl DeadlockRatingClient {
    pub fn new(api_key: Option<Secret>) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .connect_timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("cama-mm/deadlock")
            .build()
            .map_err(|_| "Could not initialize Deadlock rating HTTP client.".to_owned())?;
        Ok(Self {
            client,
            api_key,
            cache: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub async fn fetch(&self, account: u32, now: i64) -> ImportedRatings {
        if let Ok(cache) = self.cache.lock()
            && let Some((at, ratings)) = cache.get(&account)
            && at.elapsed() < Duration::from_secs(300)
        {
            return ratings.clone();
        }
        let ratings = self.fetch_uncached(account, now).await;
        if let Ok(mut cache) = self.cache.lock() {
            // Bound memory and don't retain an unbounded directory of account lookups.
            cache.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(300));
            if cache.len() >= 2048 {
                cache.clear();
            }
            cache.insert(account, (Instant::now(), ratings.clone()));
        }
        ratings
    }

    #[cfg(test)]
    pub(crate) fn seed_cache(&self, account: u32, ratings: ImportedRatings) {
        self.cache
            .lock()
            .unwrap()
            .insert(account, (Instant::now(), ratings));
    }

    async fn fetch_uncached(&self, account: u32, now: i64) -> ImportedRatings {
        let reason = match response_json(self.rank_request(account)).await {
            Ok(body) => {
                if let Some(rating) = valve_rating(&body, account, now) {
                    return ImportedRatings::from_standard(rating);
                }
                "No usable published ranked badge for this account".to_owned()
            }
            Err(reason) => format!("Rank lookup: {reason}"),
        };
        // These reasons contain only fixed messages and HTTP status codes,
        // never request URLs, response bodies, or credentials.
        tracing::warn!(%reason, "Deadlock initial rating import used a neutral prior");
        ImportedRatings::provisional(&reason)
    }

    fn rank_request(&self, account: u32) -> reqwest::RequestBuilder {
        let request = self.client.get(format!(
            "https://api.deadlock-api.com/v1/players/{account}/rank"
        ));
        match &self.api_key {
            Some(key) => request.header("X-API-Key", key.expose()),
            None => request,
        }
    }
}

async fn response_json(request: reqwest::RequestBuilder) -> Result<Value, String> {
    let mut response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            "request timed out"
        } else {
            "provider connection failed"
        }
        .to_owned()
    })?;
    if !response.status().is_success() {
        return Err(format!(
            "provider returned HTTP {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
    {
        return Err("provider response exceeded the size limit".to_owned());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "provider response could not be read".to_owned())?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("provider response exceeded the size limit".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "provider returned invalid JSON".to_owned())
}

fn badge_mu(badge: i64) -> Option<f64> {
    let tier = badge / 10;
    let subrank = badge % 10;
    if !(1..=11).contains(&tier) || !(1..=6).contains(&subrank) {
        return None;
    }
    let ordinal = (tier - 1) * 6 + subrank;
    Some(25.0 + (ordinal as f64 - 33.5) * 0.4)
}

fn valve_rating(body: &Value, account: u32, now: i64) -> Option<ImportedRating> {
    let badge = body.get("badge")?.as_i64()?;
    let mu = badge_mu(badge)?;
    let last_match = body.get("last_match")?;
    if last_match.is_null() {
        return None;
    }
    if body.get("rank")?.as_i64()? != badge / 10 || body.get("subrank")?.as_i64()? != badge % 10 {
        return None;
    }
    Some(ImportedRating {
        mu,
        sigma: 25.0 / 3.0,
        source: "valve-rank-weak-prior-v1".to_owned(),
        raw_value: Some(badge as f64),
        provenance: json!({"provider":"deadlock-api", "account_id":account,
            "badge":badge, "last_match":last_match, "fetched_at":now, "source_mode":"ranked", "transform":"badge-ordinal-v1", "provisional":true}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rank_lookup_is_keyless_with_optional_authentication() {
        let request = DeadlockRatingClient::new(None)
            .unwrap()
            .rank_request(7)
            .build()
            .unwrap();
        assert_eq!(
            request.url().as_str(),
            "https://api.deadlock-api.com/v1/players/7/rank"
        );
        assert!(!request.headers().contains_key("X-API-Key"));
        let request = DeadlockRatingClient::new(Some(Secret::new("fixture-key".to_owned())))
            .unwrap()
            .rank_request(7)
            .build()
            .unwrap();
        assert_eq!(request.headers()["X-API-Key"], "fixture-key");
    }

    #[test]
    fn steam_id_conversion_rejects_non_individual_ids() {
        assert_eq!(steam_account_id("123"), Ok(123));
        assert_eq!(
            steam_account_id(&(STEAM_ID_BASE + 123).to_string()),
            Ok(123)
        );
        for input in [
            "0",
            "-1",
            "18446744073709551615",
            "76561193665298433",
            "name",
        ] {
            assert!(steam_account_id(input).is_err(), "{input}");
        }
    }
    #[test]
    fn badge_prior_is_contiguous_and_missing_is_not_zero_skill() {
        assert!((badge_mu(21).unwrap() - badge_mu(16).unwrap() - 0.4).abs() < 1e-9);
        for badge in [0, 10, 17, 117, 121] {
            assert!(badge_mu(badge).is_none());
        }
        assert!(
            valve_rating(
                &json!({"badge":0,"rank":0,"subrank":0,"last_match":null}),
                1,
                0
            )
            .is_none()
        );
        let rating = valve_rating(
            &json!({"badge":116,"rank":11,"subrank":6,"last_match":{"match_id":2}}),
            1,
            0,
        )
        .unwrap();
        let imported = ImportedRatings::from_standard(rating);
        assert!(imported.brawl.mu > 25.0 && imported.brawl.mu < imported.standard.mu);
        assert_eq!(imported.brawl.sigma, 25.0 / 3.0);
    }
    #[test]
    fn ranked_badge_requires_consistent_fields_and_preserves_provider_provenance() {
        let body = json!({"badge":84,"rank":8,"subrank":4,"last_match":{"match_id":2}});
        let rating = valve_rating(&body, 7, 100).unwrap();
        assert_eq!(rating.raw_value, Some(84.0));
        assert_eq!(rating.mu, badge_mu(84).unwrap());
        assert_eq!(rating.provenance["provider"], "deadlock-api");
        assert_eq!(rating.provenance["account_id"], 7);
        assert_eq!(rating.provenance["source_mode"], "ranked");
        for invalid in [
            json!({"badge":84,"rank":7,"subrank":4,"last_match":{"match_id":2}}),
            json!({"badge":84,"rank":8,"subrank":3,"last_match":{"match_id":2}}),
            json!({"badge":84,"rank":8,"subrank":4,"last_match":null}),
        ] {
            assert!(valve_rating(&invalid, 7, 100).is_none());
        }
    }
    #[tokio::test]
    async fn provider_failures_are_specific_and_do_not_leak_request_credentials() {
        use std::io::{Read, Write};
        for (status, body, expected) in [
            (401, "secret response body", "provider returned HTTP 401"),
            (429, "rate limited", "provider returned HTTP 429"),
            (200, "not JSON", "provider returned invalid JSON"),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let _received = socket.read(&mut request).unwrap();
                write!(socket, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let request = reqwest::Client::new()
                .get(format!("http://{addr}/?key=fixture-secret"))
                .header("X-API-Key", "fixture-secret");
            let reason = response_json(request).await.unwrap_err();
            assert_eq!(reason, expected);
            assert!(!reason.contains("fixture-secret"));
            assert!(!reason.contains("secret response body"));
            server.join().unwrap();
        }
    }
}
