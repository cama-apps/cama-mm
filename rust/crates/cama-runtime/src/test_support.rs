use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{Connection, MAIN_DB};
use tempfile::NamedTempFile;

static MIGRATED_DATABASE_TEMPLATE: OnceLock<NamedTempFile> = OnceLock::new();
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
pub(crate) struct FastTestDatabase {
    path: PathBuf,
    _keeper: Connection,
}

impl FastTestDatabase {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

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

pub(crate) fn parity_python(root: &Path) -> Command {
    if let Some(executable) = env::var_os("CAMA_PARITY_PYTHON") {
        let executable = PathBuf::from(executable);
        let executable = if executable.is_absolute() {
            executable
        } else {
            root.join(executable)
        };
        let mut command = Command::new(executable);
        command.current_dir(root);
        command
    } else {
        let mut command = Command::new("uv");
        command
            .current_dir(root)
            .args(["run", "--locked", "python"]);
        command
    }
}
