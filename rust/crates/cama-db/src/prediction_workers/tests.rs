use std::path::PathBuf;

use crate::prediction_worker_repository::{
    PredictionRefreshPublicationKind, PredictionRefreshSettings, PredictionWorkerRepository,
};
use crate::predictions_repository::{NewLevel, PredictionRepository};
use crate::test_support::copy_migrated_database;
use rusqlite::{Connection, params};
use tempfile::TempDir;

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
