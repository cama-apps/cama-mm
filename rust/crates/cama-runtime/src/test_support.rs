use std::io;
use std::path::Path;
#[cfg(any(feature = "runtime-test-draft", feature = "runtime-test-match"))]
use std::path::PathBuf;
use std::sync::OnceLock;
#[cfg(any(feature = "runtime-test-draft", feature = "runtime-test-match"))]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(any(feature = "runtime-test-draft", feature = "runtime-test-match"))]
use rusqlite::{Connection, MAIN_DB};
use tempfile::NamedTempFile;

static MIGRATED_DATABASE_TEMPLATE: OnceLock<NamedTempFile> = OnceLock::new();
#[cfg(any(feature = "runtime-test-draft", feature = "runtime-test-match"))]
static NEXT_FAST_DATABASE_ID: AtomicU64 = AtomicU64::new(1);

fn migrated_database_template() -> &'static NamedTempFile {
    MIGRATED_DATABASE_TEMPLATE.get_or_init(|| {
        let database = NamedTempFile::new().expect("canonical database template");
        cama_db::schema_manager::initialize_or_migrate(database.path())
            .expect("migrate canonical database template");
        database
    })
}

/// Populate an isolated test database from one process-wide canonical template.
///
/// Runtime tests exercise repositories and handlers, not the migration engine.
/// Building the schema once preserves per-test SQLite isolation without replaying
/// the complete migration history for every fixture.
pub(crate) fn initialize_test_database(path: impl AsRef<Path>) -> io::Result<()> {
    std::fs::copy(migrated_database_template().path(), path).map(|_| ())
}

/// A file-backed canonical fixture for restart, locking, and durable-I/O tests.
#[cfg(any(feature = "runtime-test-draft", feature = "runtime-test-match"))]
pub(crate) fn migrated_database() -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary migrated database");
    initialize_test_database(database.path()).expect("copy canonical database template");
    database
}

/// An isolated SQLite database for runtime tests that do not verify durable I/O.
///
/// The keeper connection owns a named shared-memory database so production
/// repositories can continue opening independent connections through `path()`.
/// Dropping the handle releases the entire database without filesystem writes.
#[cfg(any(feature = "runtime-test-draft", feature = "runtime-test-match"))]
pub(crate) struct FastTestDatabase {
    path: PathBuf,
    _keeper: Connection,
}

#[cfg(any(feature = "runtime-test-draft", feature = "runtime-test-match"))]
impl FastTestDatabase {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(any(feature = "runtime-test-draft", feature = "runtime-test-match"))]
pub(crate) fn fast_database() -> FastTestDatabase {
    let id = NEXT_FAST_DATABASE_ID.fetch_add(1, Ordering::Relaxed);
    let path = PathBuf::from(format!(
        "file:cama-runtime-test-{}-{id}?mode=memory&cache=shared",
        std::process::id()
    ));
    let mut keeper = Connection::open(&path).expect("open shared-memory test database");
    keeper
        .restore(
            MAIN_DB,
            migrated_database_template().path(),
            None::<fn(rusqlite::backup::Progress)>,
        )
        .expect("restore canonical shared-memory test database");
    FastTestDatabase {
        path,
        _keeper: keeper,
    }
}

#[path = "test_support/penance_ledger_tests.rs"]
pub(crate) mod penance_ledger;

/// A scripted model for flavor tests: one fixed reply, captured requests,
/// and an optional hold that blocks every call until released.
#[cfg(any(
    feature = "runtime-test-dig",
    feature = "runtime-test-draft",
    feature = "runtime-test-match"
))]
pub(crate) struct ScriptedAi {
    reply: Option<String>,
    requests: std::sync::Mutex<Vec<cama_app::ai_services::ProviderRequest>>,
    held: std::sync::Mutex<bool>,
    released: std::sync::Condvar,
}

#[cfg(any(
    feature = "runtime-test-dig",
    feature = "runtime-test-draft",
    feature = "runtime-test-match"
))]
impl ScriptedAi {
    fn new(reply: Option<&str>, held: bool) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            reply: reply.map(str::to_owned),
            requests: std::sync::Mutex::new(Vec::new()),
            held: std::sync::Mutex::new(held),
            released: std::sync::Condvar::new(),
        })
    }

    pub(crate) fn replying(reply: &str) -> std::sync::Arc<Self> {
        Self::new(Some(reply), false)
    }

    pub(crate) fn failing() -> std::sync::Arc<Self> {
        Self::new(None, false)
    }

    /// Like `replying`, but every call blocks until `release`.
    pub(crate) fn held(reply: &str) -> std::sync::Arc<Self> {
        Self::new(Some(reply), true)
    }

    pub(crate) fn release(&self) {
        *self.held.lock().expect("scripted AI hold") = false;
        self.released.notify_all();
    }

    pub(crate) fn requests(&self) -> Vec<cama_app::ai_services::ProviderRequest> {
        self.requests.lock().expect("scripted AI requests").clone()
    }

    pub(crate) fn service(
        self: &std::sync::Arc<Self>,
    ) -> std::sync::Arc<cama_app::ai_services::AIService> {
        struct Loader(std::sync::Arc<ScriptedAi>);
        impl cama_app::ai_services::ProviderLoader for Loader {
            fn load(
                &self,
            ) -> Result<
                std::sync::Arc<dyn cama_app::ai_services::AiProvider>,
                cama_app::ai_services::ProviderError,
            > {
                Ok(self.0.clone())
            }
        }
        let config = cama_app::ai_services::ProviderConfig::new(
            "test/scripted",
            cama_app::ai_services::ProviderCredential::new("test-key").expect("test credential"),
            std::time::Duration::from_secs(30),
            500,
        )
        .expect("test AI provider config");
        std::sync::Arc::new(cama_app::ai_services::AIService::new(
            config,
            std::sync::Arc::new(Loader(std::sync::Arc::clone(self))),
        ))
    }
}

#[cfg(any(
    feature = "runtime-test-dig",
    feature = "runtime-test-draft",
    feature = "runtime-test-match"
))]
impl cama_app::ai_services::AiProvider for ScriptedAi {
    fn complete(
        &self,
        request: cama_app::ai_services::ProviderRequest,
    ) -> Result<cama_app::ai_services::ProviderResponse, cama_app::ai_services::ProviderError> {
        self.requests
            .lock()
            .expect("scripted AI requests")
            .push(request);
        let mut held = self.held.lock().expect("scripted AI hold");
        while *held {
            held = self.released.wait(held).expect("scripted AI hold");
        }
        match &self.reply {
            Some(reply) => Ok(cama_app::ai_services::ProviderResponse {
                content: Some(reply.clone()),
                ..cama_app::ai_services::ProviderResponse::default()
            }),
            None => Err(cama_app::ai_services::ProviderError::new(
                cama_app::ai_services::ProviderErrorKind::Unavailable,
                "scripted failure",
            )),
        }
    }
}
