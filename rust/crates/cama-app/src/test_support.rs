use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use tempfile::NamedTempFile;

static MIGRATED_DATABASE_TEMPLATE: OnceLock<Vec<u8>> = OnceLock::new();

fn migrated_database_template() -> &'static [u8] {
    MIGRATED_DATABASE_TEMPLATE.get_or_init(|| {
        let database = NamedTempFile::new().expect("temporary migrated database template");
        cama_db::schema_manager::initialize_or_migrate(database.path())
            .expect("initialize migrated database template");
        std::fs::read(database.path()).expect("read migrated database template")
    })
}

pub(crate) fn copy_migrated_database(path: &Path) -> std::io::Result<()> {
    std::fs::write(path, migrated_database_template())
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
