use std::env;
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
        let database = NamedTempFile::new().expect("temporary migrated database template");
        cama_db::schema_manager::initialize_or_migrate(database.path())
            .expect("initialize migrated database template");
        database
    })
}

pub(crate) fn copy_migrated_database(path: &Path) -> std::io::Result<()> {
    std::fs::copy(migrated_database_template().path(), path).map(|_| ())
}

/// An isolated shared-memory database restored from the migrated template.
///
/// The keeper lets application services open independent connections while
/// routine tests avoid durable journal and database writes.
pub(crate) struct FastTestDatabase {
    path: PathBuf,
    _keeper: Connection,
}

impl FastTestDatabase {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn fast_migrated_database() -> FastTestDatabase {
    let id = NEXT_FAST_DATABASE_ID.fetch_add(1, Ordering::Relaxed);
    let path = PathBuf::from(format!(
        "file:cama-app-test-{}-{id}?mode=memory&cache=shared",
        std::process::id()
    ));
    let mut keeper = Connection::open(&path).expect("open shared-memory test database");
    keeper
        .restore(
            MAIN_DB,
            migrated_database_template().path(),
            None::<fn(rusqlite::backup::Progress)>,
        )
        .expect("restore shared-memory test database");
    FastTestDatabase {
        path,
        _keeper: keeper,
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repository root")
        .to_path_buf()
}

pub(crate) fn parity_python() -> Command {
    let root = repository_root();
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
