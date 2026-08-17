use std::path::{Path, PathBuf};
use std::process::Command;

use crate::prediction_worker_repository::{
    PredictionRefreshPublicationKind, PredictionRefreshSettings, PredictionWorkerRepository,
};
use crate::predictions_repository::{NewLevel, PredictionRepository};
use crate::test_support::copy_migrated_database;
use rusqlite::{Connection, params};
use tempfile::{NamedTempFile, TempDir};

use super::build_levels;

const GUILD: i64 = 42;

struct Fixture {
    _directory: TempDir,
    path: PathBuf,
}

impl Fixture {
    fn migrated() -> Self {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let path = directory.path().join("cama.db");
        copy_migrated_database(&path).expect("copy migrated temporary database");
        Self {
            _directory: directory,
            path,
        }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(&self.path).expect("open fixture database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match runtime foreign-key policy");
        connection
    }

    fn create_market(&self, question: &str, last_refresh: i64) -> i64 {
        let repository = PredictionRepository::new(&self.path);
        let prediction_id = repository
            .create_orderbook_prediction(
                Some(GUILD),
                99,
                question,
                50,
                Some(500),
                &[
                    NewLevel::ask(52, 50),
                    NewLevel::ask(53, 50),
                    NewLevel::ask(54, 50),
                    NewLevel::bid(48, 50),
                    NewLevel::bid(47, 50),
                    NewLevel::bid(46, 50),
                ],
            )
            .expect("create prediction market");
        repository
            .update_discord_ids(
                prediction_id,
                Some(700 + prediction_id),
                Some(800),
                Some(900),
            )
            .expect("persist Discord IDs");
        self.connection()
            .execute(
                "UPDATE predictions SET last_refresh_at=?1,created_at=?1
                 WHERE prediction_id=?2",
                params![last_refresh, prediction_id],
            )
            .expect("set deterministic market timestamps");
        prediction_id
    }

    fn worker_repository(&self) -> PredictionWorkerRepository {
        PredictionWorkerRepository::new(&self.path)
    }
}

fn settings(refresh_seconds: i64) -> PredictionRefreshSettings {
    PredictionRefreshSettings {
        refresh_seconds,
        levels_per_side: 3,
        size_per_level: 10,
        outer_level_sizes: vec![8, 6, 4, 2],
        refresh_spread_ticks: 4,
        initial_spread_ticks: 2,
        tick_size: 1,
        fade_ticks: 5,
        price_low: 5,
        price_high: 95,
        initial_fair: 50,
        recent_trades_shown: 5,
        economy_events_enabled: false,
    }
}

#[test]
fn nonpositive_refresh_size_suppresses_outer_quotes_like_python() {
    let mut settings = settings(100);
    settings.size_per_level = 0;
    assert!(build_levels(50, 4, &settings).is_empty());
}

#[test]
fn configured_due_interval_and_refresh_outbox_commit_atomically() {
    let fixture = Fixture::migrated();
    let prediction_id = fixture.create_market("Will refresh commit?", 100);
    fixture
        .connection()
        .execute(
            "INSERT INTO prediction_trades
                 (prediction_id,discord_id,action,contracts,jopacoins,vwap_x100,
                  last_fill_price,trade_time)
             VALUES (?1,7,'buy_yes',4,20,5300,53,150)",
            [prediction_id],
        )
        .expect("seed trade since previous refresh");
    let repository = fixture.worker_repository();

    assert!(repository.due_refresh_markets(199, 100).unwrap().is_empty());
    assert_eq!(
        repository.due_refresh_markets(200, 100).unwrap()[0].prediction_id,
        prediction_id
    );
    let outcome = repository
        .refresh_market_and_queue(prediction_id, 200, 3, &settings(100))
        .unwrap()
        .expect("due market refreshed");
    assert_eq!((outcome.old_price, outcome.new_price), (50, 53));
    assert_eq!(outcome.trade_count, 1);

    let market = PredictionRepository::new(&fixture.path)
        .get_prediction(prediction_id)
        .unwrap()
        .unwrap();
    assert_eq!(market.last_refresh_at, Some(200));
    assert_eq!(market.prev_price, Some(50));
    assert_eq!(market.current_price, Some(53));
    let pending = repository.pending_refresh_publications().unwrap();
    assert_eq!(pending.len(), 2);
    assert!(matches!(
        pending[0].kind,
        PredictionRefreshPublicationKind::ThreadSummary { .. }
    ));
    assert!(matches!(
        pending[1].kind,
        PredictionRefreshPublicationKind::MarketEmbed { prediction_id: id } if id == prediction_id
    ));
}

#[test]
fn refresh_failure_rolls_back_market_and_all_publications() {
    let fixture = Fixture::migrated();
    let prediction_id = fixture.create_market("Will failure roll back?", 100);
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_worker_refresh
             BEFORE INSERT ON prediction_fair_snapshots
             WHEN NEW.reason='refresh'
             BEGIN SELECT RAISE(ABORT, 'injected refresh failure'); END;",
        )
        .expect("install failure trigger");
    let repository = fixture.worker_repository();

    assert!(
        repository
            .refresh_market_and_queue(prediction_id, 200, 1, &settings(100))
            .is_err()
    );
    let market = PredictionRepository::new(&fixture.path)
        .get_prediction(prediction_id)
        .unwrap()
        .unwrap();
    assert_eq!(market.last_refresh_at, Some(100));
    assert_eq!(market.current_price, Some(50));
    assert!(
        repository
            .pending_refresh_publications()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn refresh_acknowledgement_is_generation_safe() {
    let fixture = Fixture::migrated();
    let prediction_id = fixture.create_market("Will generations be safe?", 100);
    let repository = fixture.worker_repository();
    repository
        .refresh_market_and_queue(prediction_id, 200, 0, &settings(100))
        .unwrap();
    let publication = repository
        .pending_refresh_publications()
        .unwrap()
        .into_iter()
        .find(|publication| {
            matches!(
                publication.kind,
                PredictionRefreshPublicationKind::MarketEmbed { .. }
            )
        })
        .expect("embed publication");
    fixture
        .connection()
        .execute(
            "UPDATE app_kv SET value='newer-generation'
             WHERE guild_id=?1 AND key=?2",
            params![publication.guild_id, publication.key],
        )
        .expect("supersede publication");

    assert!(
        !repository
            .acknowledge_refresh_publication(&publication)
            .unwrap()
    );
    assert_eq!(repository.pending_refresh_publications().unwrap().len(), 1);
}

#[test]
fn digest_payload_flag_and_cursor_are_durable_and_idempotent() {
    let fixture = Fixture::migrated();
    let quiet = fixture.create_market("Quiet market?", 100);
    let active = fixture.create_market("Active market?", 100);
    fixture
        .connection()
        .execute(
            "INSERT INTO prediction_trades
                 (prediction_id,discord_id,action,contracts,jopacoins,vwap_x100,
                  last_fill_price,trade_time)
             VALUES (?1,7,'buy_yes',9,40,5200,52,950)",
            [active],
        )
        .expect("seed recent digest volume");
    fixture
        .connection()
        .execute(
            "INSERT INTO app_kv(guild_id,key,value) VALUES (?1,'split_announced','0')",
            [GUILD],
        )
        .expect("seed one-shot banner");
    let repository = fixture.worker_repository();

    let queued = repository
        .queue_digest_if_due(GUILD, 900, 1_000, 200)
        .expect("queue due digest");
    assert!(queued.queued);
    assert_eq!(queued.market_count, 2);
    let pending = repository.pending_digest_publications().unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].payload.split_banner);
    assert_eq!(pending[0].payload.markets[0].prediction_id, active);
    assert_eq!(pending[0].payload.markets[1].prediction_id, quiet);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT value FROM app_kv WHERE guild_id=?1 AND key='split_announced'",
                [GUILD],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "1"
    );

    let duplicate = repository
        .queue_digest_if_due(GUILD, 900, 1_000, 200)
        .expect("same slot is idempotent");
    assert!(!duplicate.queued);
    assert_eq!(repository.pending_digest_publications().unwrap().len(), 1);
    assert!(
        repository
            .acknowledge_digest_publication(&pending[0])
            .unwrap()
    );
    assert!(repository.pending_digest_publications().unwrap().is_empty());
}

#[test]
fn empty_digest_advances_cursor_without_creating_publication() {
    let fixture = Fixture::migrated();
    let repository = fixture.worker_repository();
    let result = repository
        .queue_digest_if_due(GUILD, 900, 1_000, 200)
        .expect("queue empty digest slot");
    assert!(!result.queued);
    assert!(repository.pending_digest_publications().unwrap().is_empty());
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT value FROM app_kv
                 WHERE guild_id=?1 AND key='prediction_digest_cursor'",
                [GUILD],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "900"
    );
}

/// Python creates a disposable current-schema prediction market, Rust refreshes
/// it and queues the durable Discord publications, and production Python reads
/// the refreshed market, order book, fair-history row, and app-kv outbox.
///
/// This is intentionally ignored with the other repository-environment
/// interop tests: it must never point at the development database, and it
/// requires the repository's Python environment.
#[test]
#[ignore = "cross-language smoke; run explicitly when repository Python is available"]
fn unmapped_python_migrated_database_prediction_worker_interop_smoke() {
    let database = NamedTempFile::new().expect("temporary interop database");
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root");
    let python = repository_root.join(".venv/bin/python");
    let initialize = Command::new(&python)
        .current_dir(&repository_root)
        .args([
            "-c",
            r#"
import sys
from infrastructure.schema_manager import SchemaManager
from repositories.prediction_repository import PredictionRepository

p = sys.argv[1]
SchemaManager(p).initialize()
r = PredictionRepository(p)
pid = r.create_orderbook_prediction(
    42,
    99,
    'Will Python read the Rust refresh?',
    50,
    500,
    [
        ('yes_ask', 52, 50),
        ('yes_ask', 53, 50),
        ('yes_ask', 54, 50),
        ('yes_bid', 48, 50),
        ('yes_bid', 47, 50),
        ('yes_bid', 46, 50),
    ],
)
r.update_prediction_discord_ids(
    pid,
    thread_id=700 + pid,
    embed_message_id=800,
    channel_message_id=900,
)
with r.connection() as connection:
    connection.execute(
        'UPDATE predictions SET last_refresh_at=100,created_at=100 WHERE prediction_id=?',
        (pid,),
    )
    connection.execute(
        'UPDATE prediction_fair_snapshots SET snapshot_at=100 WHERE market_id=?',
        (pid,),
    )
    connection.execute(
        'INSERT INTO prediction_trades '
        '(prediction_id,discord_id,action,contracts,jopacoins,vwap_x100,last_fill_price,trade_time) '
        'VALUES (?,?,?,?,?,?,?,?)',
        (pid, 7, 'buy_yes', 4, 20, 5300, 53, 150),
    )
print(pid)
"#,
        ])
        .arg(database.path())
        .output()
        .expect("run Python schema authority");
    assert!(
        initialize.status.success(),
        "Python initialization failed: {}",
        String::from_utf8_lossy(&initialize.stderr)
    );
    let prediction_id = String::from_utf8(initialize.stdout)
        .expect("Python prediction identifier")
        .trim()
        .parse::<i64>()
        .expect("numeric prediction identifier");

    let repository = PredictionWorkerRepository::new(database.path());
    let outcome = repository
        .refresh_market_and_queue(prediction_id, 200, 3, &settings(100))
        .expect("Rust refresh against Python schema")
        .expect("due prediction market");
    assert_eq!(
        (outcome.old_price, outcome.new_price, outcome.trade_count),
        (50, 53, 1)
    );
    assert_eq!(repository.pending_refresh_publications().unwrap().len(), 2);

    let verify = Command::new(&python)
        .current_dir(&repository_root)
        .args([
            "-c",
            r#"
import json
import sys
from repositories.prediction_repository import PredictionRepository

pid = int(sys.argv[2])
r = PredictionRepository(sys.argv[1])
market = r.get_prediction(pid)
assert market is not None
assert (
    market['status'],
    market['current_price'],
    market['prev_price'],
    market['last_refresh_at'],
) == ('open', 53, 50, 200), market
snapshot = r.get_market_snapshot(pid, recent_limit=5)
assert snapshot['book']['current_price'] == 53
assert snapshot['book']['yes_asks'] == [
    (52, 50), (53, 50), (57, 10), (58, 10), (59, 10),
    (60, 8), (61, 6), (62, 4), (63, 2),
], snapshot['book']['yes_asks']
assert snapshot['book']['yes_bids'] == [
    (49, 10), (48, 60), (47, 60), (46, 58),
    (45, 6), (44, 4), (43, 2),
], snapshot['book']['yes_bids']
assert snapshot['recent_trades'][0]['trade_time'] == 150
assert r.get_fair_history(pid, 42) == [(100, 50), (200, 53)]
with r.connection() as connection:
    rows = connection.execute(
        "SELECT key,value FROM app_kv "
        "WHERE guild_id=42 AND (key LIKE 'prediction_refresh_embed:%' "
        "OR key LIKE 'prediction_refresh_summary:%') ORDER BY key",
    ).fetchall()
assert len(rows) == 2, rows
assert rows[0][0] == f'prediction_refresh_embed:{pid}'
assert rows[0][1].isdigit(), rows
summary = json.loads(rows[1][1])
assert summary['prediction_id'] == pid
assert summary['thread_id'] == 700 + pid
assert '53' in summary['content'], summary
print('prediction refresh is Python-readable')
"#,
        ])
        .arg(database.path())
        .arg(prediction_id.to_string())
        .output()
        .expect("run Python post-Rust verification");
    assert!(
        verify.status.success(),
        "Python verification failed: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
}
