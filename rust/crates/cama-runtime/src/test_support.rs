use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use tempfile::NamedTempFile;

static MIGRATED_DATABASE_TEMPLATE: OnceLock<Vec<u8>> = OnceLock::new();

/// Populate an isolated test database from one process-wide canonical template.
///
/// Runtime tests exercise repositories and handlers, not the migration engine.
/// Building the schema once preserves per-test SQLite isolation without replaying
/// the complete migration history for every fixture.
pub(crate) fn initialize_test_database(path: impl AsRef<Path>) -> io::Result<()> {
    let template = MIGRATED_DATABASE_TEMPLATE.get_or_init(|| {
        let database = NamedTempFile::new().expect("canonical database template");
        cama_db::schema_manager::initialize_or_migrate(database.path())
            .expect("migrate canonical database template");
        std::fs::read(database.path()).expect("read canonical database template")
    });
    std::fs::write(path, template)
}

pub(crate) fn migrated_database() -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary migrated database");
    initialize_test_database(database.path()).expect("copy canonical database template");
    database
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
