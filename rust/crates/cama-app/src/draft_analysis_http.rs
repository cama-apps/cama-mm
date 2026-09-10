//! Optional post-match draft predictions from Batru.
//!
//! Call these blocking clients on the application's blocking worker. Batru receives
//! only the ten heroes; no account or player identifiers are sent.

use std::collections::{BTreeSet, VecDeque};
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use reqwest::blocking::{Client, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

const BATRU_URL: &str = "https://batru.gg/api/winrate";
const MAX_RESPONSE_BYTES: u64 = 1_048_576;
const WINDOW: Duration = Duration::from_secs(60);
const REQUEST_LIMIT: usize = 60;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeroDraft {
    pub radiant: Vec<i64>,
    pub dire: Vec<i64>,
}

#[derive(Debug, Error)]
pub enum DraftHttpError {
    #[error("draft request requires five distinct known heroes on each side")]
    InvalidDraft,
    #[error("draft provider request budget exhausted; retry later")]
    RateLimited,
    #[error("draft provider returned HTTP {0}")]
    Status(u16),
    #[error("draft provider returned an invalid or incomplete response")]
    InvalidResponse,
    #[error("draft provider transport failed: {0}")]
    Http(#[from] reqwest::Error),
}

#[derive(Default)]
struct RequestBudget {
    requests: VecDeque<Instant>,
    cooldown_until: Option<Instant>,
}

impl RequestBudget {
    fn retry_after(&self, now: Instant) -> Option<Duration> {
        if let Some(until) = self.cooldown_until.filter(|until| *until > now) {
            return Some(until.duration_since(now));
        }
        let mut active = self
            .requests
            .iter()
            .copied()
            .filter(|time| now.saturating_duration_since(*time) < WINDOW);
        let first = active.next()?;
        (1 + active.count() >= REQUEST_LIMIT)
            .then(|| WINDOW.saturating_sub(now.saturating_duration_since(first)))
    }

    fn admit(&mut self, now: Instant) -> Result<(), DraftHttpError> {
        if self.cooldown_until.is_some_and(|until| until > now) {
            return Err(DraftHttpError::RateLimited);
        }
        while self
            .requests
            .front()
            .is_some_and(|time| now.duration_since(*time) >= WINDOW)
        {
            self.requests.pop_front();
        }
        if self.requests.len() >= REQUEST_LIMIT {
            return Err(DraftHttpError::RateLimited);
        }
        self.requests.push_back(now);
        Ok(())
    }
}

type SharedBudget = Arc<Mutex<RequestBudget>>;

fn admit(budget: &SharedBudget) -> Result<(), DraftHttpError> {
    budget
        .lock()
        .map_err(|_| DraftHttpError::RateLimited)?
        .admit(Instant::now())
}

fn client(user_agent: &str) -> Result<Client, DraftHttpError> {
    Ok(Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(user_agent)
        .build()?)
}

fn response_json(response: Response, budget: &SharedBudget) -> Result<Value, DraftHttpError> {
    let status = response.status();
    if status.as_u16() == 429 {
        let delay = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60)
            .clamp(60, 86_400);
        let mut budget = budget.lock().map_err(|_| DraftHttpError::RateLimited)?;
        budget.cooldown_until = Some(Instant::now() + Duration::from_secs(delay));
        return Err(DraftHttpError::RateLimited);
    }
    if !status.is_success() {
        return Err(DraftHttpError::Status(status.as_u16()));
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| DraftHttpError::InvalidResponse)?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(DraftHttpError::InvalidResponse);
    }
    serde_json::from_slice(&bytes).map_err(|_| DraftHttpError::InvalidResponse)
}

#[derive(Clone)]
pub struct BatruDraftClient {
    endpoint: String,
    budget: SharedBudget,
}

impl BatruDraftClient {
    pub fn new() -> Result<Self, DraftHttpError> {
        // All instances in this process share the anonymous per-IP allowance.
        static BUDGET: OnceLock<SharedBudget> = OnceLock::new();
        Ok(Self {
            endpoint: BATRU_URL.to_owned(),
            budget: Arc::clone(
                BUDGET.get_or_init(|| Arc::new(Mutex::new(RequestBudget::default()))),
            ),
        })
    }

    /// Remaining quota cooldown without reserving or consuming a request.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        self.budget
            .lock()
            .map_or(Some(WINDOW), |budget| budget.retry_after(Instant::now()))
    }

    /// Radiant probability in basis points, preserving Batru's hundredths of a percent.
    pub fn predict(&self, draft: &HeroDraft) -> Result<i64, DraftHttpError> {
        let payload = draft_payload(draft)?;
        admit(&self.budget)?;
        // Keep creation and destruction of reqwest's blocking runtime on this
        // blocking worker too; construction of the adapter itself is async-safe.
        let client = client("cama-mm/1.0 (post-match draft analysis)")?;
        let response = client.post(&self.endpoint).json(&payload).send()?;
        parse_prediction(response_json(response, &self.budget)?)
    }
}

fn draft_payload(draft: &HeroDraft) -> Result<Value, DraftHttpError> {
    if draft.radiant.len() != 5 || draft.dire.len() != 5 {
        return Err(DraftHttpError::InvalidDraft);
    }
    let heroes: BTreeSet<_> = draft.radiant.iter().chain(&draft.dire).collect();
    if heroes.len() != 10 {
        return Err(DraftHttpError::InvalidDraft);
    }
    let side = |ids: &[i64]| -> Result<Vec<&'static str>, DraftHttpError> {
        ids.iter()
            .map(|id| hero_slug(*id).ok_or(DraftHttpError::InvalidDraft))
            .collect()
    };
    Ok(json!({"radiant": side(&draft.radiant)?, "dire": side(&draft.dire)?}))
}

#[derive(Deserialize)]
struct BatruPrediction {
    radiant_win_rate: f64,
    dire_win_rate: f64,
}

fn parse_prediction(value: Value) -> Result<i64, DraftHttpError> {
    let prediction: BatruPrediction =
        serde_json::from_value(value).map_err(|_| DraftHttpError::InvalidResponse)?;
    let radiant = prediction.radiant_win_rate;
    let dire = prediction.dire_win_rate;
    if !radiant.is_finite()
        || !dire.is_finite()
        || !(0.0..=100.0).contains(&radiant)
        || !(0.0..=100.0).contains(&dire)
        || (radiant + dire - 100.0).abs() > 0.011
    {
        return Err(DraftHttpError::InvalidResponse);
    }
    Ok((radiant * 100.0).round() as i64)
}

/// Canonical Valve internal identifiers, verified against OpenDota's constants:
/// https://github.com/odota/dotaconstants/blob/master/build/heroes.json and
/// Batru https://batru.gg/api/data/heroes (all 127 identifiers agree, 2026-09-10).
/// Fail closed for future heroes until their identifier and provider support are checked.
fn hero_slug(id: i64) -> Option<&'static str> {
    Some(match id {
        1 => "antimage",
        2 => "axe",
        3 => "bane",
        4 => "bloodseeker",
        5 => "crystal_maiden",
        6 => "drow_ranger",
        7 => "earthshaker",
        8 => "juggernaut",
        9 => "mirana",
        10 => "morphling",
        11 => "nevermore",
        12 => "phantom_lancer",
        13 => "puck",
        14 => "pudge",
        15 => "razor",
        16 => "sand_king",
        17 => "storm_spirit",
        18 => "sven",
        19 => "tiny",
        20 => "vengefulspirit",
        21 => "windrunner",
        22 => "zuus",
        23 => "kunkka",
        25 => "lina",
        26 => "lion",
        27 => "shadow_shaman",
        28 => "slardar",
        29 => "tidehunter",
        30 => "witch_doctor",
        31 => "lich",
        32 => "riki",
        33 => "enigma",
        34 => "tinker",
        35 => "sniper",
        36 => "necrolyte",
        37 => "warlock",
        38 => "beastmaster",
        39 => "queenofpain",
        40 => "venomancer",
        41 => "faceless_void",
        42 => "skeleton_king",
        43 => "death_prophet",
        44 => "phantom_assassin",
        45 => "pugna",
        46 => "templar_assassin",
        47 => "viper",
        48 => "luna",
        49 => "dragon_knight",
        50 => "dazzle",
        51 => "rattletrap",
        52 => "leshrac",
        53 => "furion",
        54 => "life_stealer",
        55 => "dark_seer",
        56 => "clinkz",
        57 => "omniknight",
        58 => "enchantress",
        59 => "huskar",
        60 => "night_stalker",
        61 => "broodmother",
        62 => "bounty_hunter",
        63 => "weaver",
        64 => "jakiro",
        65 => "batrider",
        66 => "chen",
        67 => "spectre",
        68 => "ancient_apparition",
        69 => "doom_bringer",
        70 => "ursa",
        71 => "spirit_breaker",
        72 => "gyrocopter",
        73 => "alchemist",
        74 => "invoker",
        75 => "silencer",
        76 => "obsidian_destroyer",
        77 => "lycan",
        78 => "brewmaster",
        79 => "shadow_demon",
        80 => "lone_druid",
        81 => "chaos_knight",
        82 => "meepo",
        83 => "treant",
        84 => "ogre_magi",
        85 => "undying",
        86 => "rubick",
        87 => "disruptor",
        88 => "nyx_assassin",
        89 => "naga_siren",
        90 => "keeper_of_the_light",
        91 => "wisp",
        92 => "visage",
        93 => "slark",
        94 => "medusa",
        95 => "troll_warlord",
        96 => "centaur",
        97 => "magnataur",
        98 => "shredder",
        99 => "bristleback",
        100 => "tusk",
        101 => "skywrath_mage",
        102 => "abaddon",
        103 => "elder_titan",
        104 => "legion_commander",
        105 => "techies",
        106 => "ember_spirit",
        107 => "earth_spirit",
        108 => "abyssal_underlord",
        109 => "terrorblade",
        110 => "phoenix",
        111 => "oracle",
        112 => "winter_wyvern",
        113 => "arc_warden",
        114 => "monkey_king",
        119 => "dark_willow",
        120 => "pangolier",
        121 => "grimstroke",
        123 => "hoodwink",
        126 => "void_spirit",
        128 => "snapfire",
        129 => "mars",
        131 => "ringmaster",
        135 => "dawnbreaker",
        136 => "marci",
        137 => "primal_beast",
        138 => "muerta",
        145 => "kez",
        155 => "largo",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    fn draft() -> HeroDraft {
        HeroDraft {
            radiant: vec![101, 54, 2, 33, 5],
            dire: vec![21, 92, 96, 111, 67],
        }
    }

    fn fixture_client(status: u16, body: &str) -> (BatruDraftClient, thread::JoinHandle<Value>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        let endpoint = format!("http://{}/api/winrate", listener.local_addr().unwrap());
        let body = body.to_owned();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line.trim_end(), "POST /api/winrate HTTP/1.1");
            let mut length = 0;
            let mut user_agent = false;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
                if lower.starts_with("user-agent: cama-mm/") {
                    user_agent = true;
                }
                assert!(!lower.starts_with("authorization:"));
            }
            assert!(user_agent);
            let mut request = vec![0; length];
            reader.read_exact(&mut request).unwrap();
            write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            serde_json::from_slice(&request).unwrap()
        });
        (
            BatruDraftClient {
                endpoint,
                budget: Arc::new(Mutex::new(RequestBudget::default())),
            },
            server,
        )
    }

    #[tokio::test]
    async fn adapter_can_be_constructed_and_dropped_in_async_composition() {
        let client = BatruDraftClient::new().unwrap();
        drop(client);
    }

    #[test]
    fn batru_predict_sends_canonical_heroes_only_and_preserves_percentage() {
        let (client, server) =
            fixture_client(200, r#"{"radiant_win_rate":61.66,"dire_win_rate":38.34}"#);
        assert_eq!(client.predict(&draft()).unwrap(), 6166);
        assert_eq!(
            server.join().unwrap(),
            json!({
                "radiant": ["skywrath_mage", "life_stealer", "axe", "enigma", "crystal_maiden"],
                "dire": ["windrunner", "visage", "centaur", "oracle", "spectre"]
            })
        );
    }

    #[test]
    fn malformed_partial_and_inconsistent_probabilities_are_unavailable() {
        for value in [
            json!({"radiant_win_rate":61.66}),
            json!({"radiant_win_rate":-1.0,"dire_win_rate":101.0}),
            json!({"radiant_win_rate":120.0,"dire_win_rate":-20.0}),
            json!({"radiant_win_rate":61.66,"dire_win_rate":61.66}),
            json!({"radiant_win_rate":0.6166,"dire_win_rate":0.3834}),
            json!({"radiant_win_rate":"61.66","dire_win_rate":38.34}),
            json!({"radiant_win_rate":null,"dire_win_rate":38.34}),
        ] {
            assert!(matches!(
                parse_prediction(value),
                Err(DraftHttpError::InvalidResponse)
            ));
        }
        assert_eq!(
            parse_prediction(json!({"radiant_win_rate":0,"dire_win_rate":100})).unwrap(),
            0
        );
        assert_eq!(
            parse_prediction(json!({"radiant_win_rate":100,"dire_win_rate":0})).unwrap(),
            10000
        );
    }

    #[test]
    fn draft_validation_rejects_unknown_duplicate_and_incomplete_heroes_before_http() {
        let client = BatruDraftClient {
            endpoint: "http://127.0.0.1:1".to_owned(),
            budget: Arc::new(Mutex::new(RequestBudget::default())),
        };
        let mut unknown = draft();
        unknown.radiant[0] = 99999;
        let mut duplicate = draft();
        duplicate.radiant[0] = duplicate.dire[0];
        let mut incomplete = draft();
        incomplete.radiant.pop();
        for invalid in [unknown, duplicate, incomplete] {
            assert!(matches!(
                client.predict(&invalid),
                Err(DraftHttpError::InvalidDraft)
            ));
        }
        assert!(client.budget.lock().unwrap().requests.is_empty());
    }

    #[test]
    fn canonical_mapping_covers_bundled_catalog_and_historical_internal_names() {
        let bundled: std::collections::BTreeMap<i64, String> =
            serde_json::from_str(include_str!("../data/heroes.json")).unwrap();
        let mut slugs = BTreeSet::new();
        for id in bundled.keys() {
            let slug = hero_slug(*id).unwrap_or_else(|| panic!("missing hero {id}"));
            assert!(slugs.insert(slug), "duplicate canonical slug {slug}");
        }
        assert_eq!(hero_slug(1), Some("antimage"));
        assert_eq!(hero_slug(11), Some("nevermore"));
        assert_eq!(hero_slug(108), Some("abyssal_underlord"));
        assert_eq!(hero_slug(155), Some("largo"));
        assert_eq!(hero_slug(24), None);
    }

    #[test]
    fn shared_rolling_budget_prevents_bursts_across_clones_and_recovers() {
        let now = Instant::now();
        let budget = Arc::new(Mutex::new(RequestBudget::default()));
        let other = Arc::clone(&budget);
        for _ in 0..REQUEST_LIMIT {
            budget.lock().unwrap().admit(now).unwrap();
        }
        assert!(matches!(
            other.lock().unwrap().admit(now + Duration::from_secs(59)),
            Err(DraftHttpError::RateLimited)
        ));
        other.lock().unwrap().admit(now + WINDOW).unwrap();
    }

    #[test]
    fn retry_delay_tracks_rolling_capacity_without_consuming_admission() {
        let now = Instant::now();
        let mut budget = RequestBudget::default();
        assert_eq!(budget.retry_after(now), None);
        for _ in 0..REQUEST_LIMIT - 1 {
            budget.admit(now).unwrap();
        }
        for _ in 0..3 {
            assert_eq!(budget.retry_after(now), None);
        }
        assert_eq!(budget.requests.len(), REQUEST_LIMIT - 1);
        budget.admit(now + Duration::from_secs(5)).unwrap();
        assert_eq!(
            budget.retry_after(now + Duration::from_secs(5)),
            Some(Duration::from_secs(55))
        );
        assert_eq!(
            budget.retry_after(now + Duration::from_secs(59)),
            Some(Duration::from_secs(1))
        );
        assert_eq!(budget.retry_after(now + WINDOW), None);
        assert_eq!(budget.requests.len(), REQUEST_LIMIT);
        budget.admit(now + WINDOW).unwrap();
        assert_eq!(budget.requests.len(), 2);
    }

    #[test]
    fn retry_delay_honors_cooldown_even_with_unused_capacity() {
        let now = Instant::now();
        let budget = RequestBudget {
            requests: VecDeque::new(),
            cooldown_until: Some(now + Duration::from_secs(120)),
        };
        assert_eq!(budget.retry_after(now), Some(Duration::from_secs(120)));
        assert_eq!(
            budget.retry_after(now + Duration::from_secs(61)),
            Some(Duration::from_secs(59))
        );
        assert_eq!(budget.retry_after(now + Duration::from_secs(120)), None);
        assert_eq!(budget.retry_after(now + Duration::from_secs(121)), None);
        assert!(budget.requests.is_empty());
    }

    #[test]
    fn retry_delay_is_shared_by_clones_and_handles_poisoned_budget() {
        let client = BatruDraftClient {
            endpoint: "http://127.0.0.1:1".to_owned(),
            budget: Arc::new(Mutex::new(RequestBudget::default())),
        };
        client.budget.lock().unwrap().cooldown_until = Some(Instant::now() + WINDOW);
        let other = client.clone();
        assert!(client.retry_after().is_some());
        assert!(other.retry_after().is_some());
        assert!(client.budget.lock().unwrap().requests.is_empty());
        let poisoned = Arc::clone(&client.budget);
        let _ = std::panic::catch_unwind(move || {
            let _guard = poisoned.lock().unwrap();
            panic!("poison fixture budget");
        });
        assert_eq!(other.retry_after(), Some(WINDOW));
    }

    #[test]
    fn upstream_throttling_blocks_followup_without_retrying() {
        let (client, server) = fixture_client(429, "{}");
        assert!(matches!(
            client.predict(&draft()),
            Err(DraftHttpError::RateLimited)
        ));
        server.join().unwrap();
        assert!(matches!(
            client.clone().predict(&draft()),
            Err(DraftHttpError::RateLimited)
        ));
        assert_eq!(client.budget.lock().unwrap().requests.len(), 1);
    }

    #[test]
    fn http_failure_and_malformed_json_are_not_a_split_draft() {
        for (status, body) in [(503, "{}"), (200, "not json")] {
            let (client, server) = fixture_client(status, body);
            assert!(client.predict(&draft()).is_err());
            server.join().unwrap();
        }
    }
}
