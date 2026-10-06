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

/// Fixed, deliberately weak initial prior, not a calibrated PP-to-skill model.
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
    statlocker_key: Option<Secret>,
    api_key: Option<Secret>,
    // The public profile contract has no mode selector. Never assume its PP is Brawl.
    statlocker_standard_confirmed: bool,
    cache: Arc<Mutex<BTreeMap<u32, (Instant, ImportedRatings)>>>,
}

impl DeadlockRatingClient {
    pub fn new(
        statlocker_key: Option<Secret>,
        api_key: Option<Secret>,
        statlocker_standard_confirmed: bool,
    ) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .connect_timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("cama-mm/deadlock")
            .build()
            .map_err(|_| "Could not initialize Deadlock rating HTTP client.".to_owned())?;
        Ok(Self {
            client,
            statlocker_key,
            api_key,
            statlocker_standard_confirmed,
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

    async fn fetch_uncached(&self, account: u32, now: i64) -> ImportedRatings {
        if self.statlocker_standard_confirmed
            && let Some(key) = &self.statlocker_key
        {
            let request = self
                .client
                .get(format!(
                    "https://statlocker.gg/api/public/profile/{account}"
                ))
                .header("X-API-Key", key.expose());
            if let Some(body) = response_json(request).await
                && let Some(rating) = statlocker_rating(&body, account, now)
            {
                return ImportedRatings::from_standard(rating);
            }
        }
        let mut request = self.client.get(format!(
            "https://api.deadlock-api.com/v1/players/{account}/rank"
        ));
        if let Some(key) = &self.api_key {
            request = request.header("X-API-Key", key.expose());
        }
        if let Some(body) = response_json(request).await
            && let Some(rating) = valve_rating(&body, account, now)
        {
            return ImportedRatings::from_standard(rating);
        }
        ImportedRatings::provisional(
            "No usable current external rank; missing, private, uncalibrated, or provider unavailable",
        )
    }
}

async fn response_json(request: reqwest::RequestBuilder) -> Option<Value> {
    let mut response = request.send().await.ok()?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
    {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).ok()
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

fn statlocker_rating(body: &Value, account: u32, now: i64) -> Option<ImportedRating> {
    if body.get("accountId")?.as_u64()? != u64::from(account) {
        return None;
    }
    let pp = body.get("ppScore")?.as_f64()?;
    if !pp.is_finite() || !(1.0..=100_000.0).contains(&pp) {
        return None;
    }
    let badge = body.get("estimatedRankNumber")?.as_i64()?;
    // Use supplied badge; the documentation's PP conversion is inconsistent.
    let mu = badge_mu(badge)?;
    let source_time = body.get("lastUpdated")?.as_str()?;
    let updated = chrono::DateTime::parse_from_rfc3339(source_time)
        .ok()?
        .timestamp();
    if updated > now + 300 || now.saturating_sub(updated) > 90 * 86400 {
        return None;
    }
    if body.get("isCalibrated").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    Some(ImportedRating {
        mu,
        sigma: 25.0 / 3.0,
        source: "statlocker-standard-weak-prior-v1".to_owned(),
        raw_value: Some(pp),
        provenance: json!({"provider":"statlocker", "account_id":account,
            "pp_score":pp, "badge":badge, "source_time":source_time, "fetched_at":now,
            "source_mode":"standard-operator-confirmed", "transform":"badge-ordinal-v1", "provisional":true}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn statlocker_validates_identity_freshness_and_preserves_provenance() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-02T12:00:00Z")
            .unwrap()
            .timestamp();
        let body = json!({"accountId":7,"ppScore":4250,"estimatedRankNumber":84,"lastUpdated":"2026-10-01T12:00:00Z"});
        assert!(statlocker_rating(&body, 8, now).is_none());
        let rating = statlocker_rating(&body, 7, now).unwrap();
        assert_eq!(rating.raw_value, Some(4250.0));
        assert_eq!(rating.mu, badge_mu(84).unwrap());
        assert!(statlocker_rating(&body, 7, now + 100 * 86400).is_none());
    }
}
