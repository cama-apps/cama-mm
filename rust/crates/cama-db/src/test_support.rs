use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{Connection, MAIN_DB};
use tempfile::NamedTempFile;

static MIGRATED_DATABASE_TEMPLATE: OnceLock<Vec<u8>> = OnceLock::new();
static NEXT_FAST_DATABASE_ID: AtomicU64 = AtomicU64::new(1);

fn migrated_database_template() -> &'static [u8] {
    MIGRATED_DATABASE_TEMPLATE.get_or_init(|| {
        let database = NamedTempFile::new().expect("temporary migrated database template");
        crate::schema_manager::initialize_or_migrate(database.path())
            .expect("initialize migrated database template");
        std::fs::read(database.path()).expect("read migrated database template")
    })
}

pub(crate) fn copy_migrated_database(path: &Path) -> std::io::Result<()> {
    std::fs::write(path, migrated_database_template())
}

/// An isolated shared-memory database restored from a file-backed template.
///
/// The keeper allows repositories to open their usual independent connections
/// while avoiding durable I/O in tests that do not exercise crash semantics.
pub(crate) struct FastTestDatabase {
    path: PathBuf,
    _keeper: Connection,
}

impl FastTestDatabase {
    pub(crate) fn from_template(template: &Path) -> Self {
        let id = NEXT_FAST_DATABASE_ID.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!(
            "file:cama-db-test-{}-{id}?mode=memory&cache=shared",
            std::process::id()
        ));
        let mut keeper = Connection::open(&path).expect("open shared-memory test database");
        keeper
            .restore(MAIN_DB, template, None::<fn(rusqlite::backup::Progress)>)
            .expect("restore shared-memory test database");
        Self {
            path,
            _keeper: keeper,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
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
